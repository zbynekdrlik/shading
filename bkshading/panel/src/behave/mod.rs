//! The panel's behaviour as pure, natively tested rules (fohmixer-ui's `behave`): the components
//! only render them and wire them to the browser.
//!
//! - `steppers` + `holds`: the six steppers, their ends and the optimistic hold of a tap;
//! - `block`: a block's online, registration and fps-vs-grab lines;
//! - `control_info`: why a camera cannot be controlled (shading#14), in Slovak;
//! - `preview`: when a block fetches its NDI frame and what it shows meanwhile;
//! - `arrange_drag` + `arrange_edit`: ARRANGE (shading#16).

pub mod arrange_drag;
pub mod arrange_edit;
pub mod block;
pub mod control_info;
pub mod holds;
pub mod preview;
pub mod steppers;
