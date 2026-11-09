import { memo, type ComponentPropsWithoutRef } from "react";
import { openUrl } from "@tauri-apps/plugin-opener";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";

function MarkdownLink({ href, children }: ComponentPropsWithoutRef<"a">) {
  return (
    <a
      href={href}
      onClick={(event) => {
        event.preventDefault();
        if (href && /^https?:\/\//i.test(href)) {
          void openUrl(href).catch(() => {});
        }
      }}
    >
      {children}
    </a>
  );
}

export const MarkdownText = memo(function MarkdownText({
  text,
  compact = false,
}: {
  text: string;
  compact?: boolean;
}) {
  return (
    <div className={compact ? "markdown compact" : "markdown"}>
      <ReactMarkdown
        remarkPlugins={[remarkGfm]}
        components={{ a: MarkdownLink }}
      >
        {text}
      </ReactMarkdown>
    </div>
  );
});
