// System icons: Segoe Fluent Icons on Windows (the icon font Windows 11 apps use; Segoe MDL2 Assets
// on Windows 10), Lucide at 1.5 px on macOS, per the design system.

import {
  ArrowLeft,
  Bell,
  Check,
  ChevronDown,
  ChevronRight,
  Clock,
  Copy,
  Eye,
  EyeOff,
  History,
  Info,
  Keyboard,
  Link,
  Menu,
  MessageSquare,
  Monitor,
  MoreHorizontal,
  Pause,
  Play,
  Plus,
  QrCode,
  RefreshCw,
  Search,
  Send,
  Settings,
  Shield,
  Square,
  Terminal,
  Trash2,
  TriangleAlert,
  X,
  type LucideIcon,
} from "lucide-react";
import { detectOs } from "../lib/platform";

/** Fluent icon code points (Segoe Fluent Icons), as the design's prototypes use them. */
const FLUENT = {
  menu: "E700",
  back: "E72B",
  search: "E721",
  settings: "E713",
  add: "E710",
  history: "E81C",
  pause: "E769",
  play: "E768",
  stop: "E71A",
  more: "E712",
  chevD: "E70D",
  chevR: "E76C",
  check: "E73E",
  x: "E711",
  warn: "E7BA",
  info: "E946",
  trash: "E74D",
  copy: "E8C8",
  send: "E724",
  eye: "E890",
  eyeOff: "ED1A",
  link: "E71B",
  msg: "E8BD",
  keyboard: "E765",
  clock: "E917",
  terminal: "E756",
  refresh: "E72C",
  qr: "ED14",
  shield: "EA18",
  bell: "EA8F",
  monitor: "E7F4",
} as const;

export type IconName = keyof typeof FLUENT;

const LUCIDE: Record<IconName, LucideIcon> = {
  menu: Menu,
  back: ArrowLeft,
  search: Search,
  settings: Settings,
  add: Plus,
  history: History,
  pause: Pause,
  play: Play,
  stop: Square,
  more: MoreHorizontal,
  chevD: ChevronDown,
  chevR: ChevronRight,
  check: Check,
  x: X,
  warn: TriangleAlert,
  info: Info,
  trash: Trash2,
  copy: Copy,
  send: Send,
  eye: Eye,
  eyeOff: EyeOff,
  link: Link,
  msg: MessageSquare,
  keyboard: Keyboard,
  clock: Clock,
  terminal: Terminal,
  refresh: RefreshCw,
  qr: QrCode,
  shield: Shield,
  bell: Bell,
  monitor: Monitor,
};

const mac = detectOs() === "mac";

export function Icon({ name, size = 16, className = "" }: { name: IconName; size?: number; className?: string }) {
  if (mac) {
    const L = LUCIDE[name];
    return <L className={`fi ${className}`} size={size} strokeWidth={1.5} aria-hidden="true" />;
  }
  return (
    <i className={`fi ${className}`} aria-hidden="true" style={{ fontSize: size }}>
      {String.fromCodePoint(parseInt(FLUENT[name], 16))}
    </i>
  );
}
