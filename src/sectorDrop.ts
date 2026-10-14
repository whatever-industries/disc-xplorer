export const sectorImageExtensions = ["iso", "img", "bin", "fatx", "chd", "cue", "mds", "mdx", "nrg", "ccd", "cdi", "gdi", "toc", "b5t", "b6t", "bwt", "c2d", "pdi", "gi", "daa", "cso", "ciso", "ecm", "wbfs", "wux", "wud", "sdram", "sbram", "aif", "cif", "uif", "skeleton", "skeleton.zst", "iso.zst", "img.zst"] as const;

type Bounds = { left: number; right: number; top: number; bottom: number };

export function sectorDropTarget(position: { x: number; y: number }, scale: number,
  a?: Bounds, b?: Bounds): "a" | "b" | null {
  const x = position.x / scale, y = position.y / scale;
  const contains = (r?: Bounds) => r && x >= r.left && x < r.right && y >= r.top && y < r.bottom;
  return contains(a) ? "a" : contains(b) ? "b" : null;
}
