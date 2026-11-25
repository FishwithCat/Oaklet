import oakletIcon from "../../assets/oaklet-icon.png";

export function Welcome() {
  return (
    <div className="empty">
      <img
        className="empty-logo"
        src={oakletIcon}
        alt="Oaklet"
        width={76}
        height={76}
      />
      <h1>What should we build?</h1>
    </div>
  );
}
