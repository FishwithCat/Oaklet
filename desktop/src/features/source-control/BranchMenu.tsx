import { useEffect, useRef, useState } from "react";
import {
  CheckRegular,
  DownSmallRegular,
  Refresh3Regular,
} from "@mingcute/react/core-regular";

export function BranchMenu({
  active,
  branch,
  branches,
  switchingBranch,
  branchError,
  onSwitch,
  onClearError,
}: {
  active: boolean;
  branch: string;
  branches: string[];
  switchingBranch: string | null;
  branchError: string;
  onSwitch: (branch: string) => Promise<boolean>;
  onClearError: () => void;
}) {
  const [open, setOpen] = useState(false);
  const menuRef = useRef<HTMLDivElement>(null);
  const buttonRef = useRef<HTMLButtonElement>(null);

  useEffect(() => {
    if (!open) return;

    function onPointerDown(event: PointerEvent) {
      if (
        !(event.target instanceof Node) ||
        !menuRef.current?.contains(event.target)
      ) {
        setOpen(false);
      }
    }

    function onKeyDown(event: KeyboardEvent) {
      if (event.key === "Escape") {
        setOpen(false);
        buttonRef.current?.focus();
      }
    }

    document.addEventListener("pointerdown", onPointerDown);
    document.addEventListener("keydown", onKeyDown);
    return () => {
      document.removeEventListener("pointerdown", onPointerDown);
      document.removeEventListener("keydown", onKeyDown);
    };
  }, [open]);

  useEffect(() => {
    if (!active) setOpen(false);
  }, [active]);

  async function choose(nextBranch: string) {
    if (nextBranch === branch) {
      setOpen(false);
      return;
    }
    if (await onSwitch(nextBranch)) setOpen(false);
  }

  return (
    <div className="sc-branch-wrap" ref={menuRef}>
      <button
        ref={buttonRef}
        type="button"
        className="sc-branch"
        onClick={() => {
          setOpen((current) => !current);
          onClearError();
        }}
        aria-haspopup="menu"
        aria-expanded={open}
        aria-controls={open ? "sc-branch-menu" : undefined}
        aria-label={`Switch branch, current: ${branch || "detached"}`}
        title="Switch branch"
      >
        <span className="sc-branch-name">{branch || "detached"}</span>
        <DownSmallRegular size={10} className="sc-branch-caret" />
      </button>

      {open && (
        <div
          id="sc-branch-menu"
          className="sc-branch-menu"
          role="menu"
          aria-label="Switch branch"
        >
          <div className="sc-branch-menu-title">Switch branch</div>
          {branches.length === 0 ? (
            <p className="sc-branch-empty">No local branches.</p>
          ) : (
            branches.map((item) => (
              <button
                type="button"
                role="menuitemradio"
                key={item}
                className="sc-branch-item"
                aria-checked={item === branch}
                disabled={switchingBranch !== null}
                onClick={() => void choose(item)}
                title={item}
              >
                <span>{item}</span>
                {switchingBranch === item ? (
                  <Refresh3Regular size={12} className="spin" />
                ) : item === branch ? (
                  <CheckRegular size={11} />
                ) : null}
              </button>
            ))
          )}
          {branchError && <p className="sc-branch-error">{branchError}</p>}
        </div>
      )}
    </div>
  );
}
