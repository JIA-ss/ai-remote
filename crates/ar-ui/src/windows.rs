//! Reuse uiautomation-rs (Apache-2.0), with input/screenshot/process features disabled.
//! UIA objects remain in the dedicated MTA worker for their entire lifetime.
use super::{Action, Node};
use anyhow::{Result, bail};
use uiautomation::{
    UIAutomation, UIElement,
    patterns::{
        UIExpandCollapsePattern, UIInvokePattern, UISelectionItemPattern, UITextPattern, UITogglePattern,
        UIValuePattern,
    },
};
pub const SOURCE: &str = "windows_uia";
pub struct Element(UIElement);
thread_local! { static AUTOMATION: std::cell::OnceCell<UIAutomation> = const { std::cell::OnceCell::new() }; }
fn uia_error(stage: &'static str, error: uiautomation::errors::Error) -> anyhow::Error {
    let code = error.code();
    let message = clipped(error.to_string());
    anyhow::Error::new(error)
        .context(format!("UIA {stage} failed: code={code} hex=0x{:08X} display={message:?}", code as u32))
}
fn com_error(stage: &'static str, error: windows::core::Error) -> anyhow::Error {
    let code = error.code().0;
    let message = clipped(error.to_string());
    anyhow::Error::new(error)
        .context(format!("UIA {stage} failed: code={code} hex=0x{:08X} display={message:?}", code as u32))
}
fn automation() -> Result<UIAutomation> {
    AUTOMATION.with(|a| {
        if a.get().is_none() {
            let _initial = UIAutomation::new().map_err(|error| uia_error("automation.uia_new", error))?;
            use windows::{
                Win32::{
                    System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance},
                    UI::Accessibility::{CUIAutomation8, IUIAutomation2},
                },
                core::Interface,
            };
            let bounded: IUIAutomation2 = unsafe { CoCreateInstance(&CUIAutomation8, None, CLSCTX_INPROC_SERVER) }
                .map_err(|error| com_error("automation.cocreate_uia8", error))?;
            unsafe {
                bounded.SetConnectionTimeout(500).map_err(|error| com_error("automation.connection_timeout", error))?;
                bounded
                    .SetTransactionTimeout(500)
                    .map_err(|error| com_error("automation.transaction_timeout", error))?;
                bounded.SetAutoSetFocus(false).map_err(|error| com_error("automation.auto_set_focus", error))?;
            }
            let client = UIAutomation::from(
                bounded
                    .cast::<windows::Win32::UI::Accessibility::IUIAutomation>()
                    .map_err(|error| com_error("automation.cast", error))?,
            );
            let _ = a.set(client);
        }
        Ok(a.get().unwrap().clone())
    })
}
pub fn ready() -> Result<()> {
    Ok(())
}
fn clipped(s: String) -> String {
    s.chars().take(2048).collect()
}
pub fn read(e: &Element) -> Result<Node> {
    let e = &e.0;
    let mut n = Node::empty(format!("{:?}", e.get_control_type()?));
    n.label = clipped(e.get_name()?);
    n.identifier = clipped(e.get_automation_id()?);
    // Unknown password classification is treated as protected, never expose its value or actions.
    n.protected = e.is_password().unwrap_or(true);
    n.enabled = e.is_enabled().ok();
    n.focused = e.has_keyboard_focus().ok();
    n.visible = e.is_offscreen().ok().map(|v| !v);
    n.bounds = e.get_bounding_rectangle().ok().map(|r| {
        [
            r.get_left() as f64,
            r.get_top() as f64,
            (r.get_right() - r.get_left()) as f64,
            (r.get_bottom() - r.get_top()) as f64,
        ]
    });
    if !n.protected {
        if e.get_pattern::<UIInvokePattern>().is_ok() {
            n.actions.push("press".into());
        }
        if let Ok(p) = e.get_pattern::<UIValuePattern>() {
            n.value = p.get_value().ok().map(clipped);
            n.read_only = p.is_readonly().ok();
            if matches!(p.is_readonly(), Ok(false)) {
                n.actions.push("set_value".into());
            }
        } else if let Ok(p) = e.get_pattern::<UITextPattern>() {
            n.value = p.get_document_range().ok().and_then(|r| r.get_text(2048).ok()).map(clipped);
        }
        if let Ok(p) = e.get_pattern::<UISelectionItemPattern>() {
            n.selected = p.is_selected().ok();
            n.actions.push("select".into());
        }
        if let Ok(p) = e.get_pattern::<UITogglePattern>() {
            n.checked = p.get_toggle_state().ok().and_then(|s| match format!("{s:?}").as_str() {
                "On" => Some(true),
                "Off" => Some(false),
                _ => None,
            });
            n.actions.push("toggle".into());
        }
        if let Ok(p) = e.get_pattern::<UIExpandCollapsePattern>() {
            let state = p.get_state().ok().map(|v| format!("{v:?}"));
            n.expanded = state.as_deref().and_then(|s| match s {
                "Expanded" => Some(true),
                "Collapsed" => Some(false),
                _ => None,
            });
            if state.as_deref() != Some("LeafNode") {
                n.actions.extend(["expand".into(), "collapse".into()]);
            }
        }
        if e.is_keyboard_focusable().unwrap_or(false) {
            n.actions.push("focus".into());
        }
    }
    Ok(n)
}
fn no_element(error: &uiautomation::errors::Error) -> bool {
    // A null COM result is the normal end of a child/sibling walk. Other failures stay visible.
    matches!(error.code(), 0 | 1 | 5 | -2147467261)
}
pub fn collect(pid: u32, max: usize, depth: usize, check: &dyn Fn() -> Result<()>) -> Result<super::Tree> {
    let a = automation()?;
    let walker = a.get_control_view_walker().map_err(|error| uia_error("collect.control_walker", error))?;
    let desktop = a.get_root_element().map_err(|error| uia_error("collect.desktop_root", error))?;
    let mut roots = vec![];
    if let Ok(mut w) = walker.get_first_child(&desktop) {
        loop {
            check()?;
            if w.get_process_id().ok() == Some(pid) {
                roots.push(w.clone());
            }
            match walker.get_next_sibling(&w) {
                Ok(next) => w = next,
                Err(error) if no_element(&error) => break,
                Err(error) => return Err(uia_error("collect.root_next_sibling", error)),
            }
        }
    }
    if roots.is_empty() {
        bail!("no UIA application windows for pid {pid}; provider or process unavailable");
    }
    let mut stack: Vec<_> = roots.into_iter().rev().map(|e| (e, 0, None)).collect();
    let mut nodes = vec![];
    let mut truncated = false;
    let mut warnings = vec![];
    let mut seen = std::collections::HashSet::new();
    while let Some((e, d, parent)) = stack.pop() {
        if let Err(error) = check() {
            if nodes.is_empty() {
                return Err(error);
            }
            warnings.push(error.to_string());
            truncated = true;
            break;
        }
        if nodes.len() == max {
            truncated = true;
            break;
        }
        if e.get_process_id().ok() != Some(pid) {
            warnings.push("cross-process subtree excluded".into());
            truncated = true;
            continue;
        }
        if let Ok(id) = e.get_runtime_id()
            && !id.is_empty()
            && !seen.insert(id)
        {
            continue;
        }
        let e = Element(e);
        let mut n = match read(&e) {
            Ok(n) => n,
            Err(error) => {
                warnings.push(error.to_string());
                truncated = true;
                continue;
            }
        };
        n.parent = parent;
        n.depth = d;
        let index = nodes.len();
        let child = if n.protected {
            None
        } else {
            match walker.get_first_child(&e.0) {
                Ok(child) => Some(child),
                Err(error) => {
                    if !no_element(&error) {
                        truncated = true;
                        warnings.push(error.to_string());
                    }
                    None
                }
            }
        };
        if let Some(mut child) = child {
            if d == depth {
                truncated = true;
            } else {
                let mut children = vec![];
                loop {
                    check()?;
                    children.push(child.clone());
                    if children.len() >= max {
                        truncated = true;
                        break;
                    }
                    match walker.get_next_sibling(&child) {
                        Ok(next) => child = next,
                        Err(error) => {
                            if !no_element(&error) {
                                truncated = true;
                                warnings.push(error.to_string());
                            }
                            break;
                        }
                    }
                }
                for child in children.into_iter().rev() {
                    stack.push((child, d + 1, Some(format!("e{index}"))));
                }
            }
        }
        nodes.push((n, e));
    }
    warnings.truncate(16);
    Ok((nodes, truncated, warnings))
}
pub fn act(e: &Element, action: Action, value: Option<&str>) -> Result<()> {
    match action {
        Action::Press => e.0.get_pattern::<UIInvokePattern>()?.invoke()?,
        Action::SetValue => e.0.get_pattern::<UIValuePattern>()?.set_value(value.unwrap_or(""))?,
        Action::Select => e.0.get_pattern::<UISelectionItemPattern>()?.select()?,
        Action::Toggle => e.0.get_pattern::<UITogglePattern>()?.toggle()?,
        Action::Expand => e.0.get_pattern::<UIExpandCollapsePattern>()?.expand()?,
        Action::Collapse => e.0.get_pattern::<UIExpandCollapsePattern>()?.collapse()?,
        Action::Focus => e.0.set_focus()?,
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn blank_provider_errors_retain_the_operation_and_numeric_code() {
        let code = -2147467259;
        let error = uia_error("collect.root_next_sibling", uiautomation::errors::Error::new(code, ""));
        let context = error.to_string();
        assert!(context.contains("collect.root_next_sibling"));
        assert!(context.contains("code=-2147467259"));
        assert!(context.contains("hex=0x80004005"));
        assert_eq!(error.downcast_ref::<uiautomation::errors::Error>().unwrap().code(), code);
    }
    #[test]
    fn null_com_results_end_walks_but_provider_errors_do_not() {
        for code in [0, 1, 5, -2147467261] {
            assert!(no_element(&uiautomation::errors::Error::new(code, "null element")));
        }
        for code in [-2147220991, -2147024891, -2147467259] {
            assert!(!no_element(&uiautomation::errors::Error::new(code, "provider error")));
        }
    }
}
