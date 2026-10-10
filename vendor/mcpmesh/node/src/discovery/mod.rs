//! Peer ADDRESS resolution that does not depend on external infrastructure (#68).
//!
//! Resolution answers *where*, never *who may*. A peer found by anything in here still faces the
//! trust gate exactly as one found through pkarr or handed over in an invite — which is what makes
//! it safe to add a resolver at all: a resolver cannot widen who this node admits.

pub mod local;
