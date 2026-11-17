import {
  createContext,
  memo,
  useContext,
  type ComponentPropsWithoutRef,
} from "react";
import { openUrl } from "@tauri-apps/plugin-opener";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import { inlineCodeFile } from "./file-link";

const InCodeBlock = createContext(false);
const InLink = createContext(false);

function MarkdownPre({ children }: ComponentPropsWithoutRef<"pre">) {
  return (
    <pre>
      <InCodeBlock.Provider value>{children}</InCodeBlock.Provider>
    </pre>
  );
}

function MarkdownCode({
  className,
  children,
  onOpenFile,
}: ComponentPropsWithoutRef<"code"> & {
  onOpenFile?: (href: string) => void;
}) {
  const block = useContext(InCodeBlock);
  const linked = useContext(InLink);
  const path =
    block || linked || !onOpenFile || typeof children !== "string"
      ? null
      : inlineCodeFile(children);
  if (!path) return <code className={className}>{children}</code>;
  return (
    <a
      href={path}
      className="file-link"
      title={`Open ${path}`}
      onClick={(event) => {
        event.preventDefault();
        onOpenFile?.(path);
      }}
    >
      <code className={className}>{children}</code>
    </a>
  );
}

function MarkdownLink({
  href,
  children,
  onOpenFile,
}: ComponentPropsWithoutRef<"a"> & {
  onOpenFile?: (href: string) => void;
}) {
  return (
    <a
      href={href}
      onClick={(event) => {
        event.preventDefault();
        if (href && /^https?:\/\//i.test(href)) {
          void openUrl(href).catch(() => {});
          return;
        }
        if (href) onOpenFile?.(href);
      }}
    >
      <InLink.Provider value>{children}</InLink.Provider>
    </a>
  );
}

export const MarkdownText = memo(function MarkdownText({
  text,
  compact = false,
  onOpenFile,
}: {
  text: string;
  compact?: boolean;
  onOpenFile?: (href: string) => void;
}) {
  return (
    <div className={compact ? "markdown compact" : "markdown"}>
      <ReactMarkdown
        remarkPlugins={[remarkGfm]}
        components={{
          a: (props) => <MarkdownLink {...props} onOpenFile={onOpenFile} />,
          code: (props) => <MarkdownCode {...props} onOpenFile={onOpenFile} />,
          pre: MarkdownPre,
        }}
      >
        {text}
      </ReactMarkdown>
    </div>
  );
});
