/**
 * Lyra postage-stamp mark — sawtooth (perforated) edge + serif L.
 */

interface StampMarkProps {
  size?: number;
  className?: string;
}

export function StampMark({ size = 88, className }: StampMarkProps) {
  return (
    <svg
      className={className}
      width={size}
      height={size}
      viewBox="0 0 88 88"
      fill="none"
      xmlns="http://www.w3.org/2000/svg"
      aria-hidden
    >
      <path
        d="M8 14c0-2 1.5-3.5 3.5-3.5h3.5c0-2.5 2-4.5 4.5-4.5s4.5 2 4.5 4.5h5c0-2.5 2-4.5 4.5-4.5s4.5 2 4.5 4.5h5c0-2.5 2-4.5 4.5-4.5s4.5 2 4.5 4.5h5c0-2.5 2-4.5 4.5-4.5s4.5 2 4.5 4.5h5c0-2.5 2-4.5 4.5-4.5s4.5 2 4.5 4.5H76.5c2 0 3.5 1.5 3.5 3.5v3.5c2.5 0 4.5 2 4.5 4.5s-2 4.5-4.5 4.5v5c2.5 0 4.5 2 4.5 4.5s-2 4.5-4.5 4.5v5c2.5 0 4.5 2 4.5 4.5s-2 4.5-4.5 4.5v5c2.5 0 4.5 2 4.5 4.5s-2 4.5-4.5 4.5v5c2.5 0 4.5 2 4.5 4.5s-2 4.5-4.5 4.5V74c0 2-1.5 3.5-3.5 3.5h-3.5c0 2.5-2 4.5-4.5 4.5s-4.5-2-4.5-4.5h-5c0 2.5-2 4.5-4.5 4.5s-4.5-2-4.5-4.5h-5c0 2.5-2 4.5-4.5 4.5s-4.5-2-4.5-4.5h-5c0 2.5-2 4.5-4.5 4.5s-4.5-2-4.5-4.5h-5c0 2.5-2 4.5-4.5 4.5s-4.5-2-4.5-4.5H11.5C9.5 77.5 8 76 8 74v-3.5c-2.5 0-4.5-2-4.5-4.5s2-4.5 4.5-4.5v-5c-2.5 0-4.5-2-4.5-4.5s2-4.5 4.5-4.5v-5c-2.5 0-4.5-2-4.5-4.5s2-4.5 4.5-4.5v-5c-2.5 0-4.5-2-4.5-4.5s2-4.5 4.5-4.5v-5c-2.5 0-4.5-2-4.5-4.5s2-4.5 4.5-4.5V14z"
        fill="#FFFFFF"
        stroke="#E2E2E5"
        strokeWidth="1"
      />
      <path
        d="M28 28h32M28 34h32M28 40h32"
        stroke="#1A1B1F"
        strokeWidth="1.5"
        strokeLinecap="round"
      />
      <text
        x="44"
        y="68"
        textAnchor="middle"
        fill="#1A1B1F"
        style={{ fontFamily: '"Instrument Serif", Georgia, serif', fontSize: '36px' }}
      >
        L
      </text>
    </svg>
  );
}
