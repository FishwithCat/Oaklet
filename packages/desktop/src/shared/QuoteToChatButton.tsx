export function QuoteToChatButton({ disabled, onClick }: {
  disabled: boolean;
  onClick: () => void;
}) {
  return (
    <button
      type="button"
      className="ghost quote-to-chat"
      disabled={disabled}
      title={disabled ? "Select text to quote in chat" : undefined}
      onMouseDown={(event) => event.preventDefault()}
      onClick={onClick}
    >
      Quote in Chat
    </button>
  );
}
