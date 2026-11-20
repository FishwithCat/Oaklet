/** Share the guarded close flow between the window button and application Quit. */
export function createWindowCloseHandler(actions: {
  isBusy: () => boolean;
  confirmStop: () => Promise<boolean>;
  canLeave: () => Promise<boolean>;
  prepareClose: () => Promise<void>;
  destroy: () => Promise<void>;
  onClosing: (closing: boolean) => void;
  onError: (error: unknown) => Promise<void>;
}) {
  let pending = false;
  return async () => {
    if (pending) return;
    pending = true;
    actions.onClosing(true);
    try {
      if (actions.isBusy() && !await actions.confirmStop()) return;
      if (!await actions.canLeave()) return;
      await actions.prepareClose();
      await actions.destroy();
    } catch (error) {
      await actions.onError(error);
    } finally {
      pending = false;
      actions.onClosing(false);
    }
  };
}
