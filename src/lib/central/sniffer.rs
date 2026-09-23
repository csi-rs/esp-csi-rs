//! Empty, and never to be filled: a sniffer never transmits, so it is always a peripheral
//! collector (see [`crate::model`]). Sniffer capture is driven from [`crate::CSINode::run`] via the
//! Wi-Fi driver's sniffer API. Kept only so the pre-0.11 module path still resolves.
