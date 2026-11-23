import { useEffect } from "react";
import { createPortal } from "react-dom";

/** Full-window preview for an inline image; click or Escape closes it. */
export function ImageLightbox({ src, alt, onClose }: { src: string; alt: string; onClose: () => void }) {
  useEffect(() => {
    function onKeyDown(event: KeyboardEvent) {
      if (event.key === "Escape") onClose();
    }
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [onClose]);

  return createPortal(
    <div className="modal-backdrop image-preview-backdrop" onClick={onClose}>
      <figure className="image-preview" role="dialog" aria-modal="true" aria-label={`Image preview: ${alt}`}>
        <img src={src} alt={alt} />
        <figcaption>{alt}</figcaption>
      </figure>
    </div>,
    document.body,
  );
}
