import { useState } from "react";
import { CloseRegular, FileRegular } from "@mingcute/react/core-regular";
import { ImageLightbox } from "../../shared/ImageLightbox";
import { MarkdownText } from "../../shared/Markdown";
import type { SentAttachment } from "./attachments";
import type { PendingInput } from "./session-model";
import { ProcessGroup } from "./Process";
import { toBlocks, type Item } from "./chat-model";
import type { ApprovalChoice, ToolItem } from "./process-model";
import { PlanCard } from "./PlanCard";
import {
  latestPlanVersion,
  type PlanBaseline,
  type PlanCompareData,
} from "./plan-model";

type ImageAttachment = Extract<SentAttachment, { kind: "image" }>;

function isImage(attachment: SentAttachment): attachment is ImageAttachment {
  return attachment.kind === "image";
}

function UserMessage({
  text,
  attachments,
  onPreview,
  onRemove,
}: {
  text: string;
  attachments: SentAttachment[];
  onPreview: (attachment: ImageAttachment) => void;
  onRemove?: () => void;
}) {
  const images = attachments.filter(isImage);
  const files = attachments.filter((attachment) => attachment.kind === "file");
  const imagePreviews = images.length > 0 && (
    <div className="message-images">
      {images.map((attachment, index) => (
        <button
          key={`${attachment.name}-${index}`}
          type="button"
          className="message-image"
          aria-label={`Preview ${attachment.name}`}
          title="Preview image"
          onClick={() => onPreview(attachment)}
        >
          <img src={attachment.thumbnail} alt={attachment.name} />
        </button>
      ))}
    </div>
  );
  return (
    <>
      {!onRemove && imagePreviews}
      {(text || files.length > 0 || onRemove) && (
        <div className="bubble">
          {onRemove && (
            <button
              type="button"
              className="icon-button pending-remove"
              aria-label="Remove queued message"
              onClick={onRemove}
            >
              <CloseRegular size={14} aria-hidden="true" />
            </button>
          )}
          {onRemove && imagePreviews}
          {files.length > 0 && (
            <div className="bubble-attachments">
              {files.map((attachment, index) => (
                <span
                  className="bubble-file"
                  key={`${attachment.name}-${index}`}
                >
                  <FileRegular size={12} aria-hidden="true" />
                  <span className="bubble-file-name">{attachment.name}</span>
                </span>
              ))}
            </div>
          )}
          {text && <div className="bubble-text">{text}</div>}
        </div>
      )}
    </>
  );
}

export function ChatThread({
  sessionId,
  items,
  running,
  compacting,
  activeRunId,
  onApproval,
  onConfirmPlan,
  onRevisePlan,
  onCheckPlanBaseline,
  onLoadPlanCompare,
  onOpenFile,
}: {
  sessionId: string;
  items: Item[];
  running: boolean;
  compacting: boolean;
  activeRunId: string | null;
  onApproval: (item: ToolItem, choice: ApprovalChoice) => void;
  onConfirmPlan: (version: number) => Promise<boolean>;
  onRevisePlan: (version: number) => void;
  onCheckPlanBaseline: (version: number) => Promise<PlanBaseline>;
  onLoadPlanCompare: (
    version: number,
    viewId: string,
  ) => Promise<PlanCompareData>;
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
              <UserMessage
                text={item.text}
                attachments={item.attachments ?? []}
                onPreview={setPreview}
              />
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
                  ? `${sessionId}:plan-local-${block.index}`
                  : `${sessionId}:plan-${item.seq}`
              }
              plan={item}
              latestVersion={latestVersion}
              running={running}
              onConfirm={onConfirmPlan}
              onRevise={() => onRevisePlan(item.version)}
              onCheckBaseline={onCheckPlanBaseline}
              onLoadCompare={onLoadPlanCompare}
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
        <ImageLightbox
          src={preview.thumbnail}
          alt={preview.name}
          onClose={() => setPreview(null)}
        />
      )}
    </div>
  );
}

export function PendingMessages({
  sessionId,
  pending,
  onRemovePending,
}: {
  sessionId: string;
  pending: PendingInput[];
  onRemovePending: (sessionId: string, pendingId: string) => void;
}) {
  const [preview, setPreview] = useState<ImageAttachment | null>(null);
  return (
    <section className="pending-messages" aria-label="Queued messages" aria-live="polite">
      <div className="thread">
        {pending.map((input) => (
          <div className="msg user pending" key={input.id}>
            <UserMessage
              text={input.text}
              attachments={input.attachments ?? []}
              onPreview={setPreview}
              onRemove={() => onRemovePending(sessionId, input.id)}
            />
          </div>
        ))}
      </div>
      {preview && (
        <ImageLightbox
          src={preview.thumbnail}
          alt={preview.name}
          onClose={() => setPreview(null)}
        />
      )}
    </section>
  );
}
