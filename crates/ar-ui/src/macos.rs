//! Public AXUIElement API only: no private WindowServer interfaces, synthetic clicks or OCR.
use super::{Action, Node};
use anyhow::{Result, anyhow, bail};
use core_foundation::{
    array::{CFArray, CFArrayRef},
    base::{CFHash, CFType, CFTypeRef, TCFType},
    boolean::CFBoolean,
    number::CFNumber,
    string::{CFString, CFStringRef},
};
use std::{collections::HashSet, ffi::c_void};
pub const SOURCE: &str = "macos_ax";
#[derive(Clone)]
pub struct Element(CFType);
#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXIsProcessTrusted() -> bool;
    fn AXUIElementCreateApplication(pid: i32) -> CFTypeRef;
    fn AXUIElementCopyAttributeValue(e: CFTypeRef, name: CFStringRef, value: *mut CFTypeRef) -> i32;
    fn AXUIElementCopyActionNames(e: CFTypeRef, names: *mut CFArrayRef) -> i32;
    fn AXUIElementPerformAction(e: CFTypeRef, name: CFStringRef) -> i32;
    fn AXUIElementSetAttributeValue(e: CFTypeRef, name: CFStringRef, value: CFTypeRef) -> i32;
    fn AXUIElementIsAttributeSettable(e: CFTypeRef, name: CFStringRef, settable: *mut u8) -> i32;
    fn AXUIElementSetMessagingTimeout(e: CFTypeRef, seconds: f32) -> i32;
    fn AXValueGetValue(value: CFTypeRef, kind: i32, out: *mut c_void) -> bool;
}
pub fn ready() -> Result<()> {
    if !unsafe { AXIsProcessTrusted() } {
        bail!(
            "Accessibility permission missing for the running host executable; grant it locally and restart the host"
        );
    }
    Ok(())
}
fn attr(e: &Element, name: &str) -> Option<CFType> {
    let name = CFString::new(name);
    let mut raw = std::ptr::null();
    let code = unsafe { AXUIElementCopyAttributeValue(e.0.as_CFTypeRef(), name.as_concrete_TypeRef(), &mut raw) };
    if code == 0 && !raw.is_null() { Some(unsafe { CFType::wrap_under_create_rule(raw) }) } else { None }
}
fn string(v: CFType) -> Option<String> {
    if let Some(v) = v.downcast::<CFString>() {
        Some(v.to_string().chars().take(2048).collect())
    } else if let Some(v) = v.downcast::<CFNumber>() {
        v.to_f64().map(|v| v.to_string())
    } else {
        v.downcast::<CFBoolean>().map(|v| bool::from(v).to_string())
    }
}
fn text(e: &Element, name: &str) -> String {
    attr(e, name).and_then(string).unwrap_or_default()
}
fn boolean(e: &Element, name: &str) -> Option<bool> {
    attr(e, name).and_then(|v| v.downcast::<CFBoolean>()).map(bool::from)
}
fn settable(e: &Element, name: &str) -> bool {
    let mut yes = 0;
    unsafe {
        AXUIElementIsAttributeSettable(e.0.as_CFTypeRef(), CFString::new(name).as_concrete_TypeRef(), &mut yes) == 0
            && yes != 0
    }
}
fn children(e: &Element) -> Result<Vec<Element>> {
    let mut children = Vec::new();
    // AXChildren can omit a background application's windows; union the public AXWindows list.
    let names: &[&str] =
        if text(e, "AXRole") == "AXApplication" { &["AXChildren", "AXWindows"] } else { &["AXChildren"] };
    for name in names {
        let mut raw = std::ptr::null();
        let code = unsafe {
            AXUIElementCopyAttributeValue(e.0.as_CFTypeRef(), CFString::new(name).as_concrete_TypeRef(), &mut raw)
        };
        if code == -25205 || code == -25212 {
            continue;
        }
        if code != 0 {
            bail!("AX child provider returned {code}");
        }
        if raw.is_null() {
            continue;
        }
        let value = unsafe { CFType::wrap_under_create_rule(raw) };
        if let Some(array) = value.downcast::<CFArray>() {
            for raw in array.iter().take(2001) {
                let child = Element(unsafe { CFType::wrap_under_get_rule(*raw) });
                if !children.iter().any(|old: &Element| old.0 == child.0) {
                    children.push(child);
                }
            }
        }
    }
    Ok(children)
}
fn pair(e: &Element, name: &str, kind: i32) -> Option<[f64; 2]> {
    let v = attr(e, name)?;
    let mut out = [0f64; 2];
    unsafe { AXValueGetValue(v.as_CFTypeRef(), kind, out.as_mut_ptr().cast()) }.then_some(out)
}
pub fn read(e: &Element) -> Result<Node> {
    unsafe {
        AXUIElementSetMessagingTimeout(e.0.as_CFTypeRef(), 0.15);
    }
    let role = text(e, "AXRole");
    if role.is_empty() {
        bail!("AX element is unavailable or provider did not return its role");
    }
    let mut n = Node::empty(role);
    n.identifier = text(e, "AXIdentifier");
    n.label =
        ["AXTitle", "AXDescription", "AXHelp"].iter().map(|a| text(e, a)).find(|s| !s.is_empty()).unwrap_or_default();
    let subrole = text(e, "AXSubrole");
    n.protected = subrole == "AXSecureTextField" || n.role == "AXSecureTextField";
    n.value = if n.protected { None } else { attr(e, "AXValue").and_then(string) };
    n.enabled = boolean(e, "AXEnabled");
    n.focused = boolean(e, "AXFocused");
    n.visible = boolean(e, "AXHidden").map(|v| !v).or_else(|| boolean(e, "AXVisible"));
    n.expanded = boolean(e, "AXExpanded");
    n.selected = boolean(e, "AXSelected");
    if n.role == "AXCheckBox" || n.role == "AXRadioButton" {
        n.checked = n.value.as_deref().and_then(|v| match v {
            "1" | "true" => Some(true),
            "0" | "false" => Some(false),
            _ => None,
        });
    }
    n.bounds = pair(e, "AXPosition", 1).zip(pair(e, "AXSize", 2)).map(|(p, s)| [p[0], p[1], s[0], s[1]]);
    if !n.protected {
        let mut raw = std::ptr::null();
        if unsafe { AXUIElementCopyActionNames(e.0.as_CFTypeRef(), &mut raw) } == 0 && !raw.is_null() {
            let names: CFArray<CFString> = unsafe { CFArray::wrap_under_create_rule(raw) };
            for a in names.iter() {
                let action = match a.to_string().as_str() {
                    "AXPress" => Some("press"),
                    "AXShowMenu" => None,
                    _ => None,
                };
                if let Some(a) = action {
                    n.actions.push(a.into());
                }
            }
        }
        if let Some(value) = attr(e, "AXValue") {
            let writable = settable(e, "AXValue");
            n.read_only = Some(!writable);
            if writable && value.downcast::<CFString>().is_some() {
                n.actions.push("set_value".into());
            }
        }
        if settable(e, "AXSelected") {
            n.actions.push("select".into());
        }
        if settable(e, "AXExpanded") {
            n.actions.extend(["expand".into(), "collapse".into()]);
        }
        if settable(e, "AXFocused") {
            n.actions.push("focus".into());
        }
    }
    Ok(n)
}
pub fn collect(pid: u32, max: usize, depth: usize, check: &dyn Fn() -> Result<()>) -> Result<super::Tree> {
    ready()?;
    let raw = unsafe { AXUIElementCreateApplication(pid as i32) };
    if raw.is_null() {
        bail!("could not create AX application reference");
    }
    let root = Element(unsafe { CFType::wrap_under_create_rule(raw) });
    let mut stack = vec![(root, 0, None)];
    let mut nodes = vec![];
    let mut truncated = false;
    let mut warnings = vec![];
    let mut seen = HashSet::new();
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
        // CFHash represents native AX identity; retain all references until the snapshot is dropped.
        if !seen.insert(unsafe { CFHash(e.0.as_CFTypeRef()) }) {
            continue;
        }
        let mut n = match read(&e) {
            Ok(n) => n,
            Err(error) => {
                warnings.push(error.to_string());
                truncated = true;
                continue;
            }
        };
        n.depth = d;
        n.parent = parent;
        let index = nodes.len();
        let child = if n.protected {
            vec![]
        } else {
            match children(&e) {
                Ok(c) => c,
                Err(error) => {
                    warnings.push(error.to_string());
                    truncated = true;
                    vec![]
                }
            }
        };
        if d < depth {
            for c in child.into_iter().rev() {
                stack.push((c, d + 1, Some(format!("e{index}"))));
            }
        } else if !child.is_empty() {
            truncated = true;
        }
        nodes.push((n, e));
    }
    if nodes.is_empty() {
        bail!("no readable AX elements for pid {pid}; application may have closed or lack accessibility support");
    }
    warnings.truncate(16);
    Ok((nodes, truncated, warnings))
}
pub fn act(e: &Element, action: Action, value: Option<&str>) -> Result<()> {
    let raw = e.0.as_CFTypeRef();
    let code = match action {
        Action::Press => unsafe { AXUIElementPerformAction(raw, CFString::new("AXPress").as_concrete_TypeRef()) },
        Action::SetValue => unsafe {
            AXUIElementSetAttributeValue(
                raw,
                CFString::new("AXValue").as_concrete_TypeRef(),
                CFString::new(value.unwrap_or("")).as_CFTypeRef(),
            )
        },
        other => {
            let (name, yes) = match other {
                Action::Select => ("AXSelected", true),
                Action::Expand => ("AXExpanded", true),
                Action::Collapse => ("AXExpanded", false),
                Action::Focus => ("AXFocused", true),
                _ => bail!("action unsupported"),
            };
            unsafe {
                AXUIElementSetAttributeValue(
                    raw,
                    CFString::new(name).as_concrete_TypeRef(),
                    if yes { CFBoolean::true_value() } else { CFBoolean::false_value() }.as_CFTypeRef(),
                )
            }
        }
    };
    if code != 0 {
        return Err(anyhow!("AX action returned {code}; outcome not verified, refresh state before any retry"));
    }
    Ok(())
}
