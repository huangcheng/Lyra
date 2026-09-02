import { cn } from '@/lib/utils';

/** Postage-stamp mark: ink square with serif "L" (perforated sawtooth edge). */
function stampPath(viewSize: number, teeth: number, depth: number): string {
  const v = viewSize;
  const t = v / teeth;
  const d = depth;
  const parts: string[] = [`M 0 ${d}`];

  for (let i = 0; i < teeth; i++) {
    parts.push(`L ${i * t + t / 2} 0`);
    parts.push(`L ${(i + 1) * t} ${d}`);
  }
  for (let i = 0; i < teeth; i++) {
    parts.push(`L ${v} ${d + i * t + t / 2}`);
    parts.push(`L ${v - d} ${d + (i + 1) * t}`);
  }
  for (let i = 0; i < teeth; i++) {
    parts.push(`L ${v - d - i * t - t / 2} ${v}`);
    parts.push(`L ${v - d - (i + 1) * t} ${v - d}`);
  }
  for (let i = 0; i < teeth; i++) {
    parts.push(`L ${d} ${v - d - i * t - t / 2}`);
    parts.push(`L 0 ${v - d - (i + 1) * t}`);
  }

  parts.push('Z');
  return parts.join(' ');
}

const STAMP_PATH = stampPath(32, 8, 2.5);

export function StampLogo({ size = 20, className }: { size?: number; className?: string }) {
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 32 32"
      className={cn('shrink-0', className)}
      aria-hidden
    >
      <path d={STAMP_PATH} className="fill-primary" />
      <text
        x="16"
        y="21"
        textAnchor="middle"
        className="fill-primary-foreground font-brand"
        style={{ fontSize: 18, fontWeight: 400 }}
      >
        L
      </text>
    </svg>
  );
}
