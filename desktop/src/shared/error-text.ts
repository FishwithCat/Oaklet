/**
 * Human-readable text for anything thrown or rejected by an IPC call. Keep the
 * existing fallback behavior: a non-Error object is shown as JSON.
 */
export function errorText(error: unknown): string {
  if (typeof error === "string") return error;
  if (error instanceof Error) return error.message;
  return JSON.stringify(error);
}
