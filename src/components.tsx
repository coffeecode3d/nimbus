import type { ReactNode } from "react";
import { ChevronDown } from "./icons";

export function Button({
  children,
  variant = "primary",
  onClick,
  disabled,
  wide,
  loading,
  title,
  type = "button",
}: {
  children: ReactNode;
  variant?: "primary" | "ghost" | "quiet" | "danger";
  onClick?: () => void;
  disabled?: boolean;
  wide?: boolean;
  loading?: boolean;
  title?: string;
  type?: "button" | "submit";
}) {
  return (
    <button
      type={type}
      className={`btn btn-${variant}${wide ? " btn-wide" : ""}${loading ? " is-loading" : ""}`}
      onClick={onClick}
      disabled={disabled}
      title={title}
      aria-busy={loading || undefined}
    >
      {children}
    </button>
  );
}

/** Small inline activity indicator; inherits the surrounding text colour. */
export function Spinner({ size = 13 }: { size?: number }) {
  return (
    <span
      className="spinner"
      style={{ width: size, height: size }}
      aria-hidden="true"
    />
  );
}

export function Toggle({
  checked,
  onChange,
  label,
  disabled,
}: {
  checked: boolean;
  onChange: (v: boolean) => void;
  label: string;
  disabled?: boolean;
}) {
  return (
    <button
      type="button"
      role="switch"
      aria-checked={checked}
      aria-label={label}
      disabled={disabled}
      className={`toggle${checked ? " is-on" : ""}`}
      onClick={() => onChange(!checked)}
    >
      <span className="toggle-knob" />
    </button>
  );
}

export function Cc({ code }: { code: string }) {
  return <span className="cc">{code}</span>;
}

export function LoadBar({ load }: { load: number }) {
  const tone = load < 55 ? "ok" : load < 80 ? "warn" : "bad";
  return (
    <span className={`load load-${tone}`} title={`${load}% load`}>
      <span style={{ width: `${Math.max(6, load)}%` }} />
    </span>
  );
}

export function Sparkline({
  a,
  b,
  height = 64,
}: {
  a: number[];
  b: number[];
  height?: number;
}) {
  const w = 320;
  const max = Math.max(1, ...a, ...b) * 1.18;
  const pts = (arr: number[]) =>
    arr
      .map((v, i) => `${(i / (arr.length - 1)) * w},${height - (v / max) * height}`)
      .join(" ");

  const area = (arr: number[]) =>
    `M0,${height} L${pts(arr).replaceAll(" ", " L")} L${w},${height} Z`;

  return (
    <svg
      className="spark"
      viewBox={`0 0 ${w} ${height}`}
      preserveAspectRatio="none"
      aria-hidden
    >
      <defs>
        <linearGradient id="sparkFill" x1="0" y1="0" x2="0" y2="1">
          <stop offset="0%" stopColor="var(--accent)" stopOpacity=".28" />
          <stop offset="100%" stopColor="var(--accent)" stopOpacity="0" />
        </linearGradient>
      </defs>
      <path d={area(a)} fill="url(#sparkFill)" />
      <polyline points={pts(a)} className="spark-a" />
      <polyline points={pts(b)} className="spark-b" />
    </svg>
  );
}

export function Field({
  title,
  desc,
  children,
  disabled,
}: {
  title: string;
  desc?: string;
  children: ReactNode;
  disabled?: boolean;
}) {
  return (
    <div className={`field${disabled ? " is-disabled" : ""}`}>
      <div className="field-text">
        <div className="field-title">{title}</div>
        {desc && <div className="field-desc">{desc}</div>}
      </div>
      <div className="field-control">{children}</div>
    </div>
  );
}

export function Section({
  title,
  children,
  note,
}: {
  title: string;
  children: ReactNode;
  note?: string;
}) {
  return (
    <section className="section">
      <h2 className="section-title">{title}</h2>
      <div className="section-body">{children}</div>
      {note && <p className="section-note">{note}</p>}
    </section>
  );
}

export function Select({
  value,
  onChange,
  options,
  label,
}: {
  value: string;
  onChange: (v: string) => void;
  label: string;
  options: { value: string; label: string }[];
}) {
  return (
    <div className="select">
      <select aria-label={label} value={value} onChange={(e) => onChange(e.target.value)}>
        {options.map((o) => (
          <option key={o.value} value={o.value}>
            {o.label}
          </option>
        ))}
      </select>
      <ChevronDown size={16} />
    </div>
  );
}
