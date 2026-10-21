// Addresses for Settings > Network: which of this computer's addresses to show, and whether what a
// person typed for a paired computer is something the controller can dial.

/** `ip:port` or `[ipv6]:port` split into its host part (brackets removed) and port. */
function hostPart(addr: string): string {
  const v6 = addr.match(/^\[([^\]]+)\](?::\d+)?$/);
  if (v6) return v6[1];
  const colons = addr.split(":").length - 1;
  return colons === 1 ? addr.slice(0, addr.lastIndexOf(":")) : addr;
}

function isIpv4(host: string): boolean {
  const parts = host.split(".");
  return parts.length === 4 && parts.every((p) => /^\d{1,3}$/.test(p) && Number(p) <= 255);
}

function isIpv6(host: string): boolean {
  return host.includes(":") && /^[0-9a-fA-F:.]+$/.test(host);
}

/**
 * The addresses worth giving another device: IPv4 first, then IPv6, without loopback and
 * link-local ones (they only work on this computer or this cable), duplicates removed.
 */
export function shareableAddresses(addrs: string[]): string[] {
  const keep = addrs.filter((a) => {
    const h = hostPart(a).toLowerCase();
    return !(h.startsWith("127.") || h === "::1" || h.startsWith("fe80:") || h.startsWith("169.254."));
  });
  const v4 = keep.filter((a) => isIpv4(hostPart(a)));
  const v6 = keep.filter((a) => !isIpv4(hostPart(a)));
  return [...new Set([...v4, ...v6])];
}

/**
 * Whether `text` is one or more IP addresses (`ip`, `ip:port`, `[ipv6]:port`, separated by commas
 * or spaces), the form the controller takes. Names are not looked up, so they are refused here.
 */
export function validAddressInput(text: string): boolean {
  const parts = text.split(/[\s,]+/).filter(Boolean);
  if (parts.length === 0) return false;
  return parts.every((p) => {
    const port = p.match(/^(?:\[[^\]]+\]|[^:\]]+):(\d+)$/);
    if (port && (Number(port[1]) < 1 || Number(port[1]) > 65535)) return false;
    const host = hostPart(p);
    return isIpv4(host) || isIpv6(host);
  });
}
