"""Convert whatever the user opens into a PDF the pipeline can work with."""

from __future__ import annotations

import shutil
import subprocess
from pathlib import Path

IMAGE_SUFFIXES = {".png", ".jpg", ".jpeg", ".bmp", ".gif", ".tif", ".tiff", ".webp"}
A4_INCHES = (8.27, 11.69)


class ConversionError(Exception):
    pass


def is_pdf(path: Path) -> bool:
    try:
        with open(path, "rb") as f:
            return f.read(1024).lstrip().startswith(b"%PDF")
    except OSError:
        return False


def image_to_pdf(src: Path, dest: Path) -> None:
    from PIL import Image, ImageSequence

    with Image.open(src) as im:
        frames = [f.convert("RGB") for f in ImageSequence.Iterator(im)]
    # Choose a resolution that makes the image about A4-sized rather than
    # one point per pixel.
    w, h = frames[0].size
    dpi = max(w / A4_INCHES[0], h / A4_INCHES[1], 72)
    frames[0].save(dest, "PDF", resolution=dpi, save_all=True, append_images=frames[1:])


def office_to_pdf(src: Path, workdir: Path) -> Path:
    soffice = shutil.which("soffice") or shutil.which("libreoffice")
    if not soffice:
        raise ConversionError("LibreOffice is needed to print this kind of file.")
    result = subprocess.run(
        [soffice, "--headless", "--convert-to", "pdf", "--outdir", str(workdir), str(src)],
        capture_output=True, text=True, timeout=180,
    )
    out = workdir / (src.stem + ".pdf")
    if result.returncode != 0 or not out.exists():
        raise ConversionError(f"LibreOffice could not convert {src.name}:\n{result.stderr.strip()}")
    return out


def to_pdf(src: Path, workdir: Path) -> Path:
    """Return a PDF version of ``src``. It is ``src`` itself if it's already a PDF."""
    if is_pdf(src):
        return src
    if src.suffix.lower() in IMAGE_SUFFIXES:
        dest = workdir / (src.stem + ".pdf")
        try:
            image_to_pdf(src, dest)
        except Exception as e:  # Pillow raises a variety of errors
            raise ConversionError(f"Could not read image {src.name}: {e}") from e
        return dest
    return office_to_pdf(src, workdir)
