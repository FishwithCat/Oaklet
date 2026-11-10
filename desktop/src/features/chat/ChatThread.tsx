import type { RefObject } from "react";
import { MarkdownText } from "../../shared/Markdown";
import { ProcessGroup } from "./Process";
import { toBlocks, type Item } from "./chat-model";
import type { ApprovalChoice, ToolItem } from "./process-model";

export function ChatThread({
  items,
  running,
  activeRunId,
  onApproval,
  endRef,
}: {
  items: Item[];
  running: boolean;
  activeRunId: string | null;
  onApproval: (item: ToolItem, choice: ApprovalChoice) => void;
  endRef: RefObject<HTMLDivElement | null>;
}) {
  const blocks = toBlocks(items);
  let lastProcessBlock = -1;
  for (let index = blocks.length - 1; index >= 0; index -= 1) {
    if (blocks[index]?.kind === "process") {
      lastProcessBlock = index;
      break;
    }
  }

  return (
    <div className="thread" aria-live="polite">
      {blocks.map((block, blockIndex) => {
        if (block.kind === "process") {
          return (
            <ProcessGroup
              key={`process-${block.start}`}
              items={block.items}
              live={
                running &&
                blockIndex === lastProcessBlock &&
                block.items[0]?.runId === activeRunId
              }
              onApproval={onApproval}
            />
          );
        }

        const item = block.item;
        if (item.kind === "user") {
          return (
            <div className="msg user" key={`user-${block.index}`}>
              <div className="bubble">{item.text}</div>
            </div>
          );
        }
        if (item.kind === "assistant") {
          return (
            <div className="msg assistant" key={`assistant-${block.index}`}>
              <div className="bubble">
                <MarkdownText text={item.text} />
                {item.streaming && <span className="caret" />}
              </div>
            </div>
          );
        }
        if (item.kind !== "notice") return null;
        return (
          <div className={`notice ${item.tone}`} key={`notice-${block.index}`}>
            {item.text}
          </div>
        );
      })}
      <div ref={endRef} />
    </div>
  );
}
