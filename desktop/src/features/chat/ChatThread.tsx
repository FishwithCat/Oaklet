import { FileRegular } from "@mingcute/react/core-regular";
import { MarkdownText } from "../../shared/Markdown";
import { ProcessGroup } from "./Process";
import { toBlocks, type Item } from "./chat-model";
import type { ApprovalChoice, ToolItem } from "./process-model";

export function ChatThread({
  items,
  running,
  compacting,
  activeRunId,
  onApproval,
}: {
  items: Item[];
  running: boolean;
  compacting: boolean;
  activeRunId: string | null;
  onApproval: (item: ToolItem, choice: ApprovalChoice) => void;
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
              key={
                block.items[0]?.seq === undefined
                  ? `process-local-${block.start}`
                  : `process-${block.items[0].seq}`
              }
              seq={block.items[0]?.seq}
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
            <div
              className="msg user"
              key={
                item.seq === undefined
                  ? `user-local-${block.index}`
                  : `user-${item.seq}`
              }
              data-seq={item.seq}
            >
              <div className="bubble">
                {item.attachments && item.attachments.length > 0 && (
                  <div className="bubble-attachments">
                    {item.attachments.map((attachment, index) =>
                      attachment.kind === "image" ? (
                        <img
                          key={`${attachment.name}-${index}`}
                          className="bubble-image"
                          src={attachment.thumbnail}
                          alt={attachment.name}
                          title={attachment.name}
                        />
                      ) : (
                        <span
                          className="bubble-file"
                          key={`${attachment.name}-${index}`}
                        >
                          <FileRegular size={12} aria-hidden="true" />
                          <span className="bubble-file-name">
                            {attachment.name}
                          </span>
                        </span>
                      ),
                    )}
                  </div>
                )}
                {item.text && <div className="bubble-text">{item.text}</div>}
              </div>
            </div>
          );
        }
        if (item.kind === "assistant") {
          return (
            <div
              className="msg assistant"
              key={
                item.seq === undefined
                  ? `assistant-local-${block.index}`
                  : `assistant-${item.seq}`
              }
              data-seq={item.seq}
            >
              <div className="bubble">
                <MarkdownText text={item.text} />
                {item.streaming && <span className="caret" />}
              </div>
            </div>
          );
        }
        if (item.kind !== "notice") return null;
        return (
          <div
            className={`notice ${item.tone}`}
            key={
              item.seq === undefined
                ? `notice-local-${block.index}`
                : `notice-${item.seq}`
            }
            data-seq={item.seq}
          >
            {item.text}
          </div>
        );
      })}
      {compacting && (
        <div className="notice" role="status">
          Compacting context…
        </div>
      )}
      <div />
    </div>
  );
}
