import { useState } from "react";
import type { Engine } from "../engine";
import { Button, Spinner } from "../components";
import { BrandMark, Check } from "../icons";

const MANUAL_URL = "https://airvpn.org/linux/eddie";

const STEPS = [
  "Download the signed Linux build from eddie.website.",
  "Check AirVPN's OpenPGP signature and the SHA-256 checksum.",
  "Install Eddie and its privileged helper to /opt/eddie-ui.",
];

export function EngineSetup({ engine }: { engine: Engine }) {
  const [copied, setCopied] = useState(false);
  const active = engine.installing || (engine.installStage !== null && engine.installStage !== "error");
  const percent = engine.installProgress;

  const copyManual = async () => {
    try {
      await navigator.clipboard.writeText(MANUAL_URL);
      setCopied(true);
      window.setTimeout(() => setCopied(false), 1600);
    } catch {
      setCopied(false);
    }
  };

  return (
    <div className="setup-screen">
      <div className="setup-topbar" data-tauri-drag-region />
      <div className="setup-orbit" aria-hidden="true" />
      <div className="setup-orbit setup-orbit-b" aria-hidden="true" />

      <div className="setup-card">
        <aside className="setup-aside">
          <div className="setup-brand">
            <span className="brand-mark">
              <BrandMark size={24} />
            </span>
            <div>
              <div className="brand-name">Nimbus</div>
              <div className="brand-sub">for AirVPN</div>
            </div>
          </div>

          <div className="setup-kicker">One-time setup</div>
          <h1>The interface is ready. The engine needs a home.</h1>
          <p className="setup-lede">
            Nimbus is the desktop UI. AirVPN's official Eddie engine creates the
            tunnel and manages your routes, so it is the one component we do not
            reimplement.
          </p>

          <div className="setup-aside-note">
            <span className="setup-dot" />
            <span>Unofficial client · Eddie remains AirVPN's GPL-3.0 software.</span>
          </div>
        </aside>

        <section className="setup-main">
          <div className="setup-main-head">
            <h2>Install the official engine</h2>
            <p>
              One click downloads the stable Linux build, verifies it, and asks
              for administrator approval once.
            </p>
          </div>

          <ol className="setup-steps">
            {STEPS.map((text, index) => (
              <li key={text}>
                <span className="setup-step-number">{index + 1}</span>
                <span>{text}</span>
              </li>
            ))}
          </ol>

          <Button
            wide
            onClick={() => void engine.installEngine()}
            disabled={engine.installing}
            loading={engine.installing}
          >
            {engine.installing ? (
              <>
                <Spinner />
                Installing…
              </>
            ) : engine.installError ? (
              "Try installation again"
            ) : (
              "Install official engine"
            )}
          </Button>

          <p className="setup-meta">
            Linux x86_64 · about 34 MB · requires polkit and GnuPG
          </p>

          {active && (
            <div className="setup-progress" role="status" aria-live="polite">
              <div className="setup-progress-head">
                <span>{engine.installMessage ?? "Preparing…"}</span>
                {typeof percent === "number" && <span className="mono">{percent}%</span>}
              </div>
              <div className="setup-progress-track">
                <span
                  className={typeof percent === "number" ? "is-determinate" : ""}
                  style={typeof percent === "number" ? { width: `${percent}%` } : undefined}
                />
              </div>
            </div>
          )}

          {engine.installError && (
            <div className="setup-error" role="alert">
              <strong>Setup did not finish</strong>
              <p>{engine.installError}</p>
              <p className="setup-error-hint">
                You can install Eddie yourself from the official download page,
                then return here and check again.
              </p>
            </div>
          )}

          <div className="setup-manual">
            <div className="setup-manual-row">
              <div>
                <strong>Already installed Eddie?</strong>
                <span>Nimbus will use the existing engine.</span>
              </div>
              <Button
                variant="quiet"
                onClick={() => void engine.refreshEngine()}
                disabled={engine.installing}
              >
                Check again
              </Button>
            </div>
            <div className="setup-manual-url">
              <code>{MANUAL_URL}</code>
              <button type="button" className="setup-copy" onClick={() => void copyManual()}>
                {copied ? <Check size={13} /> : "Copy"}
                {copied ? "Copied" : null}
              </button>
            </div>
          </div>
        </section>
      </div>

      <p className="setup-legal">
        Nimbus never bundles or modifies Eddie. The installer only accepts an
        archive signed by AirVPN's pinned release key.
      </p>
    </div>
  );
}
