/**
 * Files and images pasted into the composer.
 *
 * The browser hands us `File` objects without paths, so nothing can be handed
 * to the file tools. Images travel to the model as OpenAI-style image parts,
 * and text is inlined into the user message. Both are capped so one paste
 * cannot blow up the context window or the history quota.
 */

/** Longest edge kept for a pasted image; providers downscale around here too. */
const MAX_IMAGE_EDGE = 1568;
const THUMBNAIL_EDGE = 96;
const IMAGE_QUALITY = 0.9;
const MAX_TEXT_BYTES = 200_000;
const IMAGE_NAME = /\.(png|jpe?g|gif|webp|bmp|avif)$/i;

/** A prepared attachment waiting in the composer. */
export type Attachment =
  | {
      kind: "image";
      name: string;
      bytes: number;
      /** Full image sent to the model. */
      dataUrl: string;
      /** Small copy kept in chat history for the bubble. */
      thumbnail: string;
    }
  | { kind: "file"; name: string; bytes: number; text: string };

/**
 * The display-only copy stored on a sent message. File contents and full
 * images already live in the model-facing message, so keep the history light.
 */
export type SentAttachment =
  | { kind: "image"; name: string; bytes: number; thumbnail: string }
  | { kind: "file"; name: string; bytes: number };

export function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  const kb = bytes / 1024;
  if (kb < 1024) return `${kb < 10 ? kb.toFixed(1) : Math.round(kb)} KB`;
  const mb = kb / 1024;
  return `${mb < 10 ? mb.toFixed(1) : Math.round(mb)} MB`;
}

async function loadImage(url: string, name: string): Promise<HTMLImageElement> {
  const image = new Image();
  image.src = url;
  await image.decode();
  if (!image.naturalWidth || !image.naturalHeight) {
    throw new Error(`Could not read ${name || "that image"}.`);
  }
  return image;
}

function readText(file: File): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => resolve(String(reader.result ?? ""));
    reader.onerror = () =>
      reject(new Error(`Could not read ${file.name || "that file"}.`));
    reader.readAsText(file);
  });
}

function encodeImage(
  image: HTMLImageElement,
  maxEdge: number,
  quality: number,
): string {
  const scale = Math.min(
    1,
    maxEdge / Math.max(image.naturalWidth, image.naturalHeight),
  );
  const canvas = document.createElement("canvas");
  canvas.width = Math.max(1, Math.round(image.naturalWidth * scale));
  canvas.height = Math.max(1, Math.round(image.naturalHeight * scale));
  const context = canvas.getContext("2d");
  if (!context) throw new Error("Could not prepare the image.");
  // Transparent pixels would turn black in JPEG; flatten them onto white.
  context.fillStyle = "#ffffff";
  context.fillRect(0, 0, canvas.width, canvas.height);
  context.drawImage(image, 0, 0, canvas.width, canvas.height);
  return canvas.toDataURL("image/jpeg", quality);
}

/** Read one pasted file into something the composer and the model can use. */
export async function readAttachment(file: File): Promise<Attachment> {
  if (file.type.startsWith("image/") || IMAGE_NAME.test(file.name)) {
    const url = URL.createObjectURL(file);
    try {
      const image = await loadImage(url, file.name);
      return {
        kind: "image",
        name: file.name || "Pasted image",
        bytes: file.size,
        dataUrl: encodeImage(image, MAX_IMAGE_EDGE, IMAGE_QUALITY),
        thumbnail: encodeImage(image, THUMBNAIL_EDGE, 0.8),
      };
    } catch {
      throw new Error(`Could not read ${file.name || "that image"}.`);
    } finally {
      URL.revokeObjectURL(url);
    }
  }

  if (file.size > MAX_TEXT_BYTES) {
    throw new Error(
      `${file.name} is larger than ${formatBytes(MAX_TEXT_BYTES)}. ` +
        "Put it in the workspace and mention its path instead.",
    );
  }
  const text = await readText(file);
  if (text.includes("\0")) {
    throw new Error(
      `${file.name} is not a text file. Images and text files can be pasted.`,
    );
  }
  return {
    kind: "file",
    name: file.name || "Pasted file",
    bytes: file.size,
    text,
  };
}

export function sentAttachments(
  attachments: readonly Attachment[],
): SentAttachment[] {
  return attachments.map((attachment) =>
    attachment.kind === "image"
      ? {
          kind: "image",
          name: attachment.name,
          bytes: attachment.bytes,
          thumbnail: attachment.thumbnail,
        }
      : {
          kind: "file",
          name: attachment.name,
          bytes: attachment.bytes,
        },
  );
}

/**
 * Build the `content` of a user message. Text is kept as a plain string until
 * an attachment needs the multimodal part array.
 */
export function userContent(
  text: string,
  attachments: readonly Attachment[],
): string | Array<Record<string, unknown>> {
  if (attachments.length === 0) return text;
  const parts: Array<Record<string, unknown>> = [];
  if (text) parts.push({ type: "text", text });
  for (const attachment of attachments) {
    if (attachment.kind === "image") {
      parts.push({
        type: "image_url",
        image_url: { url: attachment.dataUrl },
      });
    } else {
      parts.push({
        type: "text",
        text: `Attached file ${attachment.name}:\n\n${attachment.text}`,
      });
    }
  }
  return parts;
}
