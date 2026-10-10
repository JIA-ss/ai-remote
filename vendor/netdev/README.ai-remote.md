# netdev iOS build patch

Source: crates.io netdev 0.46.3, upstream https://github.com/shellrow/netdev,
revision `4505d238696975a97b029c1b8607487453c0f86e`. MIT license retained.
Only `src/os/darwin/ipv6_addr_flags.rs` changes: libc 0.2.190 exposes `in6_ifreq`
on macOS only. The iOS build uses a local repr(C) request with the same 288-byte
Darwin ABI, keeping the IPv6 flags query and failure fallback. The macOS query stays
on libc's type. A native macOS test compares size/alignment/member offset.

ABI reference: https://github.com/apple-oss-distributions/xnu/blob/main/bsd/netinet6/in6_var.h
The existing SIOCGIFAFLAG_IN6 ioctl encodes the same size. iOS builds are checked in CI.
Remove this patch after an upstream iOS fix is available and verified.
