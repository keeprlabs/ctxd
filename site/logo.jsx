/* ctxd logo — connecting-nodes graph mark + wordmark
 *
 * 5 nodes (4 leaves + 1 hub) connected by edges. Reads as
 * "context graph" / "addressable nodes". Pure monochrome —
 * the mark renders in whatever currentColor the parent provides.
 */

const Mark = ({ size = 32, color = "currentColor", accent, ...rest }) => {
  const a = accent || color;
  return (
    <svg width={size} height={size} viewBox="0 0 32 32" fill="none" {...rest}>
      <g stroke={color} strokeWidth="1.5" strokeLinecap="round">
        <line x1="6"  y1="8"  x2="16" y2="16" />
        <line x1="16" y1="16" x2="26" y2="8"  />
        <line x1="16" y1="16" x2="6"  y2="24" />
        <line x1="16" y1="16" x2="26" y2="24" />
      </g>
      <circle cx="6"  cy="8"  r="2.4" fill={color} />
      <circle cx="26" cy="8"  r="2.4" fill={color} />
      <circle cx="6"  cy="24" r="2.4" fill={color} />
      <circle cx="26" cy="24" r="2.4" fill={color} />
      <circle cx="16" cy="16" r="3.6" fill={a} />
    </svg>
  );
};

const Wordmark = ({ size = 24, color = "currentColor", weight = 600, tracking = "-0.04em", ...rest }) => (
  <span style={{ fontFamily: "var(--font-mono)", fontWeight: weight, fontSize: size, letterSpacing: tracking, color, lineHeight: 1, display: "inline-block" }} {...rest}>
    ctxd
  </span>
);

const Lockup = ({ size = 24, color = "currentColor", accent, gap = 10, ...rest }) => (
  <span style={{ display: "inline-flex", alignItems: "center", gap, color, lineHeight: 1 }} {...rest}>
    <Mark size={size * 1.15} color={color} accent={accent} />
    <Wordmark size={size} color={color} />
  </span>
);

Object.assign(window, { Mark, Wordmark, Lockup });
