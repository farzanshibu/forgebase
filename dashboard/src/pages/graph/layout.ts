/** Small deterministic force-directed layout (no dependency): repulsion between all nodes, springs along edges, light gravity. */
export interface Pos { x: number; y: number }

export function layout(ids: string[], links: [string, string][], prev: Map<string, Pos>, width: number, height: number, iterations = 120): Map<string, Pos> {
  const pos = new Map<string, Pos>();
  const n = ids.length;
  ids.forEach((id, i) => {
    const p = prev.get(id);
    if (p) pos.set(id, { ...p });
    else {
      // New nodes start on a circle (deterministic), near their already placed neighbour when there is one.
      const a = (2 * Math.PI * i) / Math.max(n, 1);
      const anchor = links.map(([s, t]) => (s === id ? prev.get(t) : t === id ? prev.get(s) : undefined)).find(Boolean);
      pos.set(id, anchor ? { x: anchor.x + 40 * Math.cos(a), y: anchor.y + 40 * Math.sin(a) } : { x: width / 2 + (width / 3) * Math.cos(a), y: height / 2 + (height / 3) * Math.sin(a) });
    }
  });
  const pinned = new Set(ids.filter((id) => prev.has(id)));
  const k = Math.sqrt((width * height) / Math.max(n, 1)) * 0.6;
  for (let it = 0; it < iterations; it++) {
    const temp = 1 - it / iterations;
    const disp = new Map<string, Pos>(ids.map((id) => [id, { x: 0, y: 0 }]));
    for (let i = 0; i < n; i++)
      for (let j = i + 1; j < n; j++) {
        const a = pos.get(ids[i]!)!, b = pos.get(ids[j]!)!;
        let dx = a.x - b.x, dy = a.y - b.y;
        let d = Math.hypot(dx, dy);
        if (d < 0.01) { dx = 0.01 * (i + 1); dy = 0.01 * (j + 1); d = Math.hypot(dx, dy); }
        const f = (k * k) / d;
        disp.get(ids[i]!)!.x += (dx / d) * f; disp.get(ids[i]!)!.y += (dy / d) * f;
        disp.get(ids[j]!)!.x -= (dx / d) * f; disp.get(ids[j]!)!.y -= (dy / d) * f;
      }
    for (const [s, t] of links) {
      const a = pos.get(s), b = pos.get(t);
      if (!a || !b || s === t) continue;
      const dx = a.x - b.x, dy = a.y - b.y, d = Math.hypot(dx, dy) || 0.01;
      const f = (d * d) / k;
      disp.get(s)!.x -= (dx / d) * f; disp.get(s)!.y -= (dy / d) * f;
      disp.get(t)!.x += (dx / d) * f; disp.get(t)!.y += (dy / d) * f;
    }
    for (const id of ids) {
      const p = pos.get(id)!, d = disp.get(id)!;
      d.x += (width / 2 - p.x) * 0.05; d.y += (height / 2 - p.y) * 0.05;
      // Nodes already on screen move less so the view stays steady while it expands.
      const cap = (pinned.has(id) ? 6 : 30) * temp + 0.5;
      const len = Math.hypot(d.x, d.y) || 1;
      p.x = Math.min(width - 20, Math.max(20, p.x + (d.x / len) * Math.min(len, cap)));
      p.y = Math.min(height - 20, Math.max(20, p.y + (d.y / len) * Math.min(len, cap)));
    }
  }
  return pos;
}
