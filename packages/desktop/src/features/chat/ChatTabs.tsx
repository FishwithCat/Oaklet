import {
  AddRegular,
  CloseRegular,
  Message4Regular,
} from "@mingcute/react/core-regular";
import { FolderGit2Filled } from "@mingcute/react/core-filled";
import { useTabDrag } from "../../shared/useTabDrag";
import type { WorktreeInfo } from "./WorktreeControl";
import type { ChatTabInfo } from "./session-model";

const STATUS_LABEL: Record<ChatTabInfo["status"], string> = {
  working: "Working",
  idle: "Idle",
  done: "Done",
  block: "Blocked",
};

export function ChatTabs({
  hidden,
  tabs,
  activeTabId,
  onSelectTab,
  onNewTab,
  onMoveTab,
  onCloseTab,
  worktrees,
}: {
  hidden: boolean;
  tabs: ChatTabInfo[];
  activeTabId: string;
  onSelectTab: (id: string) => void;
  onNewTab: () => void;
  onMoveTab: (from: number, to: number) => void;
  onCloseTab: (id: string) => void;
  worktrees?: Record<string, WorktreeInfo>;
}) {
  const { draggingId, dragProps } = useTabDrag(onMoveTab);

  return (
    <header className="view-bar tab-bar" hidden={hidden}>
      <span className="view-bar-icon" aria-hidden="true">
        <Message4Regular size={14} />
      </span>
      <div className="view-tabs" role="tablist" aria-label="Chats">
        {tabs.map((tab) => {
          const info = worktrees?.[tab.id];
          const branch = info?.head?.branch;
          const marker = info?.checkout
            ? `Worktree${branch ? ` · ${branch}` : ""}\n${info.checkout.path}`
            : "";
          return (
            <div
              key={tab.id}
              className={`view-tab${
                tab.id === activeTabId ? " active" : ""
              }${tab.id === draggingId ? " dragging" : ""}`}
              {...dragProps(tab.id)}
            >
              {!info?.checkout && (
                <span
                  className={`tab-status ${tab.status}`}
                  role="img"
                  aria-label={`Status: ${STATUS_LABEL[tab.status]}`}
                  title={STATUS_LABEL[tab.status]}
                />
              )}
              <button
                type="button"
                role="tab"
                aria-selected={tab.id === activeTabId}
                className="view-tab-label"
                title={marker ? `${tab.title}\n${marker}` : tab.title}
                aria-label={marker ? `${tab.title}, worktree, ${STATUS_LABEL[tab.status]}` : undefined}
                onClick={() => onSelectTab(tab.id)}
              >
                {info?.checkout && (
                  <FolderGit2Filled
                    className={`view-tab-worktree ${tab.status}`}
                    size={12}
                    aria-hidden="true"
                  />
                )}
                <span>{tab.title}</span>
              </button>
              <button
                type="button"
                className="view-tab-close"
                onClick={() => onCloseTab(tab.id)}
                disabled={tab.running}
                aria-label={`Close ${tab.title}`}
                title={
                  tab.running
                    ? "Stop the run before closing"
                    : "Close chat"
                }
              >
                <CloseRegular size={11} />
              </button>
            </div>
          );
        })}
      </div>
      <button
        type="button"
        className="ghost icon-button view-add"
        onClick={onNewTab}
        aria-label="New chat"
        title="New chat"
      >
        <AddRegular size={14} />
      </button>
    </header>
  );
}
