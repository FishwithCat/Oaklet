import { Message4Regular } from "@mingcute/react/core-regular";
import type { CSSProperties, ReactNode } from "react";
import type { useSelectionAction } from "./useSelectionAction";

export function SelectionActionButton({ selection, title, children, onAction }: {
  selection: ReturnType<typeof useSelectionAction>;
  title?: string;
  children: ReactNode;
  onAction: (text: string) => void;
}) {
  const { selectionAction, selectionButtonRef, clearSelection } = selection;
  if (!selectionAction) return null;
  return (
    <button
      ref={selectionButtonRef}
      type="button"
      className={`selection-action${selectionAction.above ? "" : " below"}`}
      style={{ "--selection-x": `${selectionAction.x}px`, "--selection-y": `${selectionAction.y}px` } as CSSProperties}
      onMouseDown={(event) => event.preventDefault()}
      onClick={() => {
        clearSelection();
        onAction(selectionAction.text);
      }}
      title={title}
    >
      {children}
    </button>
  );
}

export function QuoteToChatButton({ selection, onAction }: {
  selection: ReturnType<typeof useSelectionAction>;
  onAction: (text: string) => void;
}) {
  return (
    <SelectionActionButton selection={selection} onAction={onAction}>
      <Message4Regular size={13} aria-hidden="true" />
      Quote in Chat
    </SelectionActionButton>
  );
}
