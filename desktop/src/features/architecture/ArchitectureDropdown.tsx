import { useEffect, useRef, useState, type KeyboardEvent } from "react";
import { CaretDownIcon, CheckIcon } from "@phosphor-icons/react";

export function ArchitectureDropdown({
  label,
  value,
  options,
  onChange,
}: {
  label: string;
  value: string;
  options: { value: string; label: string; title?: string }[];
  onChange: (value: string) => void;
}) {
  const [open, setOpen] = useState(false);
  const [active, setActive] = useState(0);
  const wrapRef = useRef<HTMLDivElement>(null);
  const triggerRef = useRef<HTMLButtonElement>(null);
  const itemsRef = useRef<Array<HTMLButtonElement | null>>([]);
  const selected =
    options.find((option) => option.value === value) ?? options[0];

  useEffect(() => {
    if (!open) return;
    itemsRef.current[active]?.focus();
  }, [open]);

  useEffect(() => {
    if (!open) return;
    function onPointerDown(event: PointerEvent) {
      if (!wrapRef.current?.contains(event.target as Node)) setOpen(false);
    }
    document.addEventListener("pointerdown", onPointerDown);
    return () => document.removeEventListener("pointerdown", onPointerDown);
  }, [open]);

  function show() {
    const index = options.findIndex((option) => option.value === value);
    setActive(index < 0 ? 0 : index);
    setOpen(true);
  }

  function close(refocus = false) {
    setOpen(false);
    if (refocus) triggerRef.current?.focus();
  }

  function focusItem(index: number) {
    const next = Math.max(0, Math.min(options.length - 1, index));
    setActive(next);
    itemsRef.current[next]?.focus();
  }

  function choose(option: { value: string }) {
    onChange(option.value);
    close(true);
  }

  function onTriggerKeyDown(event: KeyboardEvent<HTMLButtonElement>) {
    if (open || !["ArrowDown", "ArrowUp", "Enter", " "].includes(event.key)) {
      return;
    }
    event.preventDefault();
    show();
  }

  function onMenuKeyDown(event: KeyboardEvent<HTMLDivElement>) {
    if (event.key === "Escape") {
      event.preventDefault();
      close(true);
    } else if (event.key === "ArrowDown") {
      event.preventDefault();
      focusItem(active + 1);
    } else if (event.key === "ArrowUp") {
      event.preventDefault();
      focusItem(active - 1);
    } else if (event.key === "Home") {
      event.preventDefault();
      focusItem(0);
    } else if (event.key === "End") {
      event.preventDefault();
      focusItem(options.length - 1);
    } else if (event.key === "Tab") {
      close(true);
    }
  }

  return (
    <div className="arch-dropdown" ref={wrapRef}>
      <button
        ref={triggerRef}
        type="button"
        className="arch-select"
        aria-haspopup="menu"
        aria-expanded={open}
        aria-label={label}
        onClick={() => (open ? close() : show())}
        onKeyDown={onTriggerKeyDown}
      >
        <span className="arch-select-label">{selected?.label ?? value}</span>
        <CaretDownIcon size={10} className="arch-select-caret" />
      </button>
      {open && (
        <div
          className="arch-menu"
          role="menu"
          aria-label={label}
          onKeyDown={onMenuKeyDown}
        >
          {options.map((option, index) => (
            <button
              key={option.value}
              ref={(element) => {
                itemsRef.current[index] = element;
              }}
              type="button"
              role="menuitemradio"
              aria-checked={option.value === value}
              className="arch-menu-item"
              title={option.title}
              onClick={() => choose(option)}
            >
              <span>{option.label}</span>
              {option.value === value && <CheckIcon size={11} weight="bold" />}
            </button>
          ))}
        </div>
      )}
    </div>
  );
}
