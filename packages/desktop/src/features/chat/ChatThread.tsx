import { useEffect, useState } from "react";
import { FileRegular } from "@mingcute/react/core-regular";
import { MarkdownText } from "../../shared/Markdown";
import type { SentAttachment } from "./attachments";
import { ProcessGroup } from "./Process";
import { toBlocks, type Item } from "./chat-model";
import type { ApprovalChoice, ToolItem } from "./process-model";
import { PlanCard } from "./PlanCard";
import { latestPlanVersion } from "./plan-model";

type ImageAttachment = Extract<SentAttachment, { kind: "image" }>;

function isImage(attachment: SentAttachment): attachment is ImageAttachment {
  return attachment.kind === "image";
}

export function ChatThread({
  items,
  running,
  compacting,
  activeRunId,
  onApproval,
  onConfirmPlan,
  onRevisePlan,
  onOpenFile,
}: {
  items: Item[];
  running: boolean;
  compacting: boolean;
  activeRunId: string | null;
  onApproval: (item: ToolItem, choice: ApprovalChoice) => void;
  onConfirmPlan: (version: number) => Promise<boolean>;
  onRevisePlan: (version: number) => void;
  onOpenFile: (href: string) => void;
}) {
  const [preview, setPreview] = useState<ImageAttachment | null>(null);
  const blocks = toBlocks(items);
  const latestVersion = latestPlanVersion(items);
  let lastProcessBlock = -1;
  for (let index = blocks.length - 1; index >= 0; index -= 1) {
    if (blocks[index]?.kind === "process") {
      lastProcessBlock = index;
      break;
    }
  }

  useEffect(() => {
    if (!preview) return;
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") setPreview(null);
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [preview]);

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
          const attachments = item.attachments ?? [];
          const images = attachments.filter(isImage);
          const files = attachments.filter(
            (attachment) => attachment.kind === "file",
          );
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
              {images.length > 0 && (
                <div className="message-images">
                  {images.map((attachment, index) => (
                    <button
                      key={`${attachment.name}-${index}`}
                      type="button"
                      className="message-image"
                      aria-label={`Preview ${attachment.name}`}
                      title="Preview image"
                      onClick={() => setPreview(attachment)}
                    >
                      <img src={attachment.thumbnail} alt={attachment.name} />
                    </button>
                  ))}
                </div>
              )}
              {(item.text || files.length > 0) && (
                <div className="bubble">
                  {files.length > 0 && (
                    <div className="bubble-attachments">
                      {files.map((attachment, index) => (
                        <span
                          className="bubble-file"
                          key={`${attachment.name}-${index}`}
                        >
                          <FileRegular size={12} aria-hidden="true" />
                          <span className="bubble-file-name">
                            {attachment.name}
                          </span>
                        </span>
                      ))}
                    </div>
                  )}
                  {item.text && <div className="bubble-text">{item.text}</div>}
                </div>
              )}
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
                <MarkdownText text={item.text} onOpenFile={onOpenFile} />
              </div>
            </div>
          );
        }
        if (item.kind === "plan") {
          return (
            <PlanCard
              key={
                item.seq === undefined
                  ? `plan-local-${block.index}`
                  : `plan-${item.seq}`
              }
              plan={item}
              latestVersion={latestVersion}
              running={running}
              onConfirm={onConfirmPlan}
              onRevise={() => onRevisePlan(item.version)}
              onOpenFile={onOpenFile}
            />
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
      {preview && (
        <div
          className="modal-backdrop image-preview-backdrop"
          onClick={() => setPreview(null)}
        >
          <figure
            className="image-preview"
            role="dialog"
            aria-modal="true"
            aria-label={`Image preview: ${preview.name}`}
          >
            <img src={preview.thumbnail} alt={preview.name} />
            <figcaption>{preview.name}</figcaption>
          </figure>
        </div>
      )}
    </div>
  );
}
