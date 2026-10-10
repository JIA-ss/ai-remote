use super::{Action, Node};
use anyhow::{Result, bail};
pub const SOURCE: &str = "unsupported";
pub struct Element;
pub fn ready() -> Result<()> {
    bail!("native UI tools support macOS and Windows only")
}
pub fn read(_: &Element) -> Result<Node> {
    bail!("unsupported")
}
pub fn collect(_: u32, _: usize, _: usize, _: &dyn Fn() -> Result<()>) -> Result<super::Tree> {
    bail!("unsupported")
}
pub fn act(_: &Element, _: Action, _: Option<&str>) -> Result<()> {
    bail!("unsupported")
}
