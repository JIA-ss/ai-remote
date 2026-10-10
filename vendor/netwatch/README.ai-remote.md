# netwatch iOS build patch

Source: crates.io netwatch 0.19.3, upstream https://github.com/n0-computer/net-tools,
revision `0030e57fc70981895bb86212ff0f059cc41c8a4e` (netwatch directory).
MIT OR Apache-2.0 licenses are copied from the same upstream revision.

libc 0.2.190 omits Darwin routing constants on iOS although netwatch's BSD backend
requires them. `src/ios_libc.rs` re-exports libc and supplies only the absent constants
with their existing Darwin values. Three BSD files select that alias on iOS. No route
parsing layouts or socket behavior change; non-iOS targets still use libc directly.

Reference: https://github.com/apple-oss-distributions/xnu/blob/main/bsd/net/route.h
The Rust iOS target check and full SDK/simulator build validate this patch. Physical-device
network transitions still require acceptance. Remove when an upstream fix is verified.
