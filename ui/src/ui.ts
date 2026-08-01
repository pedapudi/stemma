/* stemma console — TypeScript, no framework, no npm.
 * Source of truth for ui/static/ui.js; build with ui/build.sh (deno). */

/* ---------- API payload types (mirror proto/stemma/v1/resolve.proto and
 * the stemmadb Python client's JSON) ---------- */

interface TraceToken {
  text: string;
  start: number;
  end: number;
  stopword: boolean;
}

interface TraceChannelScore {
  channel: string;
  rank: number;
  raw: number;
}

interface TraceCandidate {
  table: string;
  column: string;
  rowid: number | string; // proto int64 arrives as a string in JSON
  value: string;
  value_truncated: boolean;
  score: number;
  selected: boolean;
  reject_reason: string;
  channels: TraceChannelScore[];
}

interface TraceSpan {
  id: number;
  text: string;
  start: number;
  end: number;
  status: string;
  candidates: TraceCandidate[];
}

interface Trace {
  query: string;
  elapsed_ms: number;
  tokens: TraceToken[];
  spans: TraceSpan[];
  mentions: number[];
}

interface ColumnInfo {
  name: string;
  type: string;
  pk: boolean;
  notnull: boolean;
}

interface TableInfo {
  name: string;
  row_count: number;
  columns: ColumnInfo[];
  foreign_keys: { from_column: string; to_table: string; to_column: string }[];
}

interface Schema {
  tables: TableInfo[];
}

interface RowsPage {
  columns: string[];
  rows: (string | number | null)[][];
  total: number;
  offset: number;
}

interface GraphNode {
  id: string;
  kind: string;
  rows: number;
  columns: string[];
}

interface GraphEdge {
  source: string;
  target: string;
  label: string;
}

interface SchemaGraph {
  layer: string;
  nodes: GraphNode[];
  edges: GraphEdge[];
}

interface StoreMeta {
  exists: boolean;
  path?: string;
  schema_version?: number;
  size_bytes?: number;
  model_registry?: Record<string, string | number>[];
  embed_queue?: number;
  lexical?: { values: number; tables: number; columns: number } | null;
}

interface SqlResult {
  columns: string[];
  rows: (string | number | null)[][];
  truncated: boolean;
  elapsed_ms: number;
  detail?: string;
}

interface Health {
  grpc: boolean;
  latency_ms: number;
}

/* ---------- DOM helpers ---------- */

type Child = Node | string | number | null | undefined | Child[];

function el<K extends keyof HTMLElementTagNameMap>(
  tag: K,
  attrs?: Record<string, unknown> | null,
  ...children: Child[]
): HTMLElementTagNameMap[K] {
  const n = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs ?? {})) {
    if (v === null || v === undefined) continue;
    if (k === "class") n.className = String(v);
    else if (k.startsWith("on") && typeof v === "function") {
      n.addEventListener(k.slice(2), v as EventListener);
    } else n.setAttribute(k, String(v));
  }
  appendChildren(n, children);
  return n;
}

function svgEl(tag: string, attrs?: Record<string, unknown> | null, ...children: Child[]): SVGElement {
  const n = document.createElementNS("http://www.w3.org/2000/svg", tag) as SVGElement;
  for (const [k, v] of Object.entries(attrs ?? {})) n.setAttribute(k, String(v));
  appendChildren(n, children);
  return n;
}

function appendChildren(n: Element, children: Child[]): void {
  for (const c of children) {
    if (c === null || c === undefined) continue;
    if (Array.isArray(c)) appendChildren(n, c);
    else n.append(c instanceof Node ? c : document.createTextNode(String(c)));
  }
}

async function getJSON<T>(url: string): Promise<T> {
  const r = await fetch(url);
  if (!r.ok) {
    let detail = r.statusText;
    try {
      detail = ((await r.json()) as { detail?: string }).detail ?? detail;
    } catch (_e) { /* keep statusText */ }
    throw new Error(detail);
  }
  return r.json() as Promise<T>;
}

function esc(s: unknown): string {
  return String(s).replace(/[&<>"']/g, (c) =>
    ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c] as string)
  );
}

/* singleton hovercard */
const hovercard = document.getElementById("hovercard") as HTMLDivElement;

function hov(node: Element, html: string): void {
  node.addEventListener("mouseenter", () => {
    hovercard.innerHTML = html;
    hovercard.classList.add("on");
  });
  node.addEventListener("mousemove", (ev) => {
    const e = ev as MouseEvent;
    const pad = 14;
    let x = e.clientX + pad, y = e.clientY + pad;
    const r = hovercard.getBoundingClientRect();
    if (x + r.width > innerWidth - 8) x = e.clientX - r.width - pad;
    if (y + r.height > innerHeight - 8) y = e.clientY - r.height - pad;
    hovercard.style.left = `${x}px`;
    hovercard.style.top = `${y}px`;
  });
  node.addEventListener("mouseleave", () => hovercard.classList.remove("on"));
}

/* ---------- state & chrome ---------- */

const state: { dbs: string[]; db: string | null; schema: Schema | null; view: string } = {
  dbs: [],
  db: null,
  schema: null,
  view: "resolve",
};

const THEMES = [
  "paper", "solarized-light", "google-light", "lunaria-light", "belafonte-day",
  "monokai", "solarized-dark", "google-dark", "lunaria-eclipse", "belafonte-night",
  "zenburn", "selenized-black", "relaxed", "espresso", "dracula", "ubuntu",
] as const;

function initTheme(): void {
  const saved = localStorage.getItem("stemma.theme") ?? "paper";
  document.documentElement.dataset.theme = saved;
  const box = document.getElementById("swatches") as HTMLDivElement;
  for (const t of THEMES) {
    const b = el("button", {
      class: "swatch" + (t === saved ? " on" : ""),
      "data-swatch": t,
      "aria-label": t,
      onclick: () => {
        document.documentElement.dataset.theme = t;
        localStorage.setItem("stemma.theme", t);
        box.querySelectorAll(".swatch").forEach((s) => s.classList.remove("on"));
        b.classList.add("on");
      },
    });
    b.style.background = "linear-gradient(135deg, var(--paper) 55%, var(--accent) 55%)";
    hov(b, esc(t));
    box.append(b);
  }
  (document.getElementById("themebtn") as HTMLButtonElement).addEventListener("click", (e) => {
    e.stopPropagation();
    box.classList.toggle("open");
  });
  document.addEventListener("click", () => box.classList.remove("open"));
}

async function pollHealth(): Promise<void> {
  const s = document.getElementById("status") as HTMLSpanElement;
  const w = document.getElementById("statusword") as HTMLSpanElement;
  try {
    const h = await getJSON<Health>("/api/health");
    s.className = "status " + (h.grpc ? "ok" : "down");
    w.textContent = h.grpc ? "live" : "grpc down";
  } catch (_e) {
    s.className = "status down";
    w.textContent = "ui down";
  }
  setTimeout(pollHealth, 8000);
}

function renderSidebar(): void {
  const side = document.getElementById("sidebar") as HTMLElement;
  side.replaceChildren();
  if (state.dbs.length > 1) {
    side.append(el("div", { class: "tree-group" }, "database"));
    for (const d of state.dbs) {
      side.append(el("button", {
        class: "tree-node" + (d === state.db ? " sel" : ""),
        onclick: () => {
          state.db = d;
          state.schema = null;
          route();
        },
      }, el("span", { class: "tree-icon" }), d));
    }
  }
  side.append(el("div", { class: "tree-group" }, "tables"));
  if (!state.schema) {
    side.append(el("div", { class: "empty" }, "loading…"));
    return;
  }
  const current = location.hash.startsWith("#/data/")
    ? decodeURIComponent(location.hash.slice(7).split("?")[0])
    : null;
  for (const t of state.schema.tables) {
    side.append(el("button", {
      class: "tree-node" + (t.name === current ? " sel" : ""),
      onclick: () => {
        location.hash = "#/data/" + encodeURIComponent(t.name);
      },
    },
      el("span", { class: "tree-icon round" }),
      t.name,
      el("span", { class: "count" }, t.row_count.toLocaleString())));
  }
}

function setCrumbs(...parts: (string | undefined)[]): void {
  (document.getElementById("crumbs") as HTMLElement).textContent =
    [state.db, ...parts].filter(Boolean).join(" · ");
}

/* ---------- resolve view (the trajectory) ---------- */

const EXAMPLES = [
  "the Q3 numbers for the Seattle office",
  "what did Chen's team ship",
  "revenue at Northgate",
];

function viewResolve(host: HTMLElement, params: URLSearchParams): void {
  const q = params.get("q") ?? "";
  setCrumbs("resolve");
  const input = el("input", {
    class: "input",
    value: q,
    placeholder: "ask about the data — mentions resolve to records…",
    onkeydown: (e: Event) => {
      if ((e as KeyboardEvent).key === "Enter") run(input.value);
    },
  });
  const out = el("div", null);
  host.append(
    el("h1", { class: "h1" }, "resolve"),
    el("p", { class: "lede" },
      "a query names things obliquely — the trajectory below shows how each span of the query was considered, which retrieval channels fired, and every candidate record: chosen and near-miss alike."),
    el("div", { class: "queryrow" },
      input,
      el("button", { class: "btn accent", onclick: () => run(input.value) }, "resolve")),
    el("div", null, EXAMPLES.map((x) =>
      el("button", {
        class: "chip",
        style: "margin-right:6px; cursor:pointer",
        onclick: () => {
          input.value = x;
          run(x);
        },
      }, x))),
    out,
  );
  if (q) run(q);

  async function run(query: string): Promise<void> {
    if (!query.trim()) return;
    history.replaceState(null, "", "#/resolve?q=" + encodeURIComponent(query));
    (document.getElementById("topsearch") as HTMLInputElement).value = query;
    out.replaceChildren(el("div", { class: "empty" }, "resolving…"));
    let trace: Trace;
    try {
      trace = await getJSON<Trace>(
        `/api/db/${state.db}/resolve?q=` + encodeURIComponent(query));
    } catch (e) {
      out.replaceChildren(
        el("div", { class: "sql-error" }, "resolution failed — " + (e as Error).message));
      return;
    }
    renderTrace(out, trace);
  }
}

function renderTrace(out: HTMLElement, trace: Trace): void {
  out.replaceChildren();
  const mentionSpans = trace.mentions.map((i) => trace.spans[i]);

  /* the query line: tokens, mentions marked */
  const qline = el("div", { class: "qline" });
  const covered = (pos: number) => mentionSpans.find((s) => pos >= s.start && pos < s.end);
  let cursor = 0;
  const tokenNodes = new Map<number, HTMLElement>(); // span.id -> first token node
  for (const t of trace.tokens) {
    if (t.start > cursor) qline.append(trace.query.slice(cursor, t.start));
    const m = covered(t.start);
    const cls = "qtok" + (m ? " mention" : t.stopword ? " stop" : "");
    const node = el("span", { class: cls }, t.text);
    if (m && !tokenNodes.has(m.id)) tokenNodes.set(m.id, node);
    qline.append(node);
    cursor = t.end;
  }
  if (cursor < trace.query.length) qline.append(trace.query.slice(cursor));

  /* mention lanes */
  const lanes = el("div", { class: "lanes" });
  const laneNodes = new Map<number, HTMLElement>();
  for (const s of mentionSpans) {
    const lane = el("div", { class: "lane" },
      el("div", { class: "lane-head" },
        el("span", { class: "lane-span" }, s.text),
        el("span", { class: "lane-pos" }, `bytes ${s.start}–${s.end}`),
        el("span", { class: "lane-pos" },
          `${s.candidates.length} candidate${s.candidates.length === 1 ? "" : "s"}`)),
      s.candidates.map((c, i) => renderCandidate(c, i)));
    laneNodes.set(s.id, lane);
    lanes.append(lane);
  }
  if (!mentionSpans.length) {
    lanes.append(el("div", { class: "empty" },
      "— no mentions resolved; every span is in the considered list below"));
  }

  const traj = el("div", { class: "traj" }, qline, lanes);
  const wires = svgEl("svg", { class: "wires", "aria-hidden": "true" });
  traj.prepend(wires);

  /* spans considered but not selected — the near-miss ledger */
  const also = trace.spans
    .filter((s) => s.status !== "selected" && s.status !== "skipped")
    .sort((a, b) => a.start - b.start);
  const alsoBox = el("div", { class: "alsoran section" },
    el("div", { class: "subhead" }, `spans considered · ${also.length}`));
  const statusPill: Record<string, [string, string]> = {
    overlapped: ["neutral", "overlapped"],
    weak: ["caution", "weak"],
    no_candidates: ["neutral", "no match"],
  };
  for (const s of also) {
    const [tone, label] = statusPill[s.status] ?? ["neutral", s.status];
    const row = el("div", { class: "alsoran-row" },
      el("span", { class: "spantext" }, `“${s.text}”`),
      el("span", { class: "pill " + tone }, label),
      el("span", { class: "alsoran-cands" },
        s.candidates.length
          ? s.candidates.map((c) =>
            `${c.table}.${c.column} #${c.rowid} “${c.value}” (${c.score.toFixed(2)})`).join(" · ")
          : "—"));
    if (s.candidates.length) {
      hov(row, s.candidates.map((c) =>
        `<b>${esc(c.table)}.${esc(c.column)}</b> #${c.rowid} “${esc(c.value)}”<br>` +
        `score ${c.score.toFixed(3)} · ${esc(c.reject_reason)}`).join("<hr>"));
    }
    alsoBox.append(row);
  }
  if (!also.length) {
    alsoBox.append(el("div", { class: "empty" }, "— every considered span became a mention"));
  }

  out.append(
    el("div", { class: "sql-caption" },
      `resolved in ${trace.elapsed_ms.toFixed(1)} ms · ${trace.spans.length} spans enumerated · channels: exact, bm25, trigram`),
    traj,
    alsoBox,
  );

  /* wires: query span -> its lane, drawn after layout */
  requestAnimationFrame(() => {
    const box = traj.getBoundingClientRect();
    wires.setAttribute("viewBox", `0 0 ${box.width} ${box.height}`);
    for (const s of mentionSpans) {
      const tok = tokenNodes.get(s.id), lane = laneNodes.get(s.id);
      if (!tok || !lane) continue;
      const a = tok.getBoundingClientRect(), b = lane.getBoundingClientRect();
      const x1 = a.left - box.left + a.width / 2, y1 = a.bottom - box.top + 2;
      const x2 = b.left - box.left + 22, y2 = b.top - box.top;
      wires.append(svgEl("path", {
        class: "wire",
        d: `M ${x1} ${y1} C ${x1} ${y1 + 28}, ${x2} ${y2 - 28}, ${x2} ${y2}`,
      }));
    }
  });
}

function renderCandidate(c: TraceCandidate, rank: number): HTMLElement {
  const row = el("div", { class: "cand " + (c.selected ? (rank === 0 ? "sel-0" : "sel") : "rej") },
    el("span", { class: "cand-id" },
      `${c.table}.${c.column} `, el("span", { class: "rowid" }, `#${c.rowid}`)),
    el("span", { class: "cand-val", title: c.value },
      `“${c.value}${c.value_truncated ? "…" : ""}”`),
    el("span", { class: "cand-right" },
      el("span", { class: "cand-chips" },
        c.channels.map((ch) => el("span", { class: "chip" }, `${ch.channel} №${ch.rank + 1}`)),
        c.selected ? null : el("span", { class: "pill bad" },
          (c.reject_reason || "rejected").replace(/_/g, " "))),
      el("span", { style: "display:flex; gap:6px; align-items:center" },
        el("span", { class: "meter" },
          el("span", { style: `width:${Math.round(c.score * 100)}%` })),
        el("span", { class: "score" }, c.score.toFixed(2)))));
  hov(row, c.channels.map((ch) =>
    `<b>${esc(ch.channel)}</b> rank ${ch.rank + 1} · raw ${ch.raw.toFixed(3)}`).join("<br>") +
    (c.selected ? "" : `<hr>${esc(c.reject_reason.replace(/_/g, " "))}`));
  return row;
}

/* ---------- data view ---------- */

async function viewData(host: HTMLElement, params: URLSearchParams, table?: string): Promise<void> {
  const tables = state.schema?.tables ?? [];
  const name = table ?? tables[0]?.name;
  setCrumbs("data", name);
  if (!name) {
    host.append(el("div", { class: "empty" }, "— no tables in this database"));
    return;
  }
  const limit = 50;
  let offset = Number(params.get("offset") ?? 0);
  const meta = tables.find((t) => t.name === name);
  const body = el("div", null);
  host.append(
    el("h1", { class: "h1" }, name),
    el("p", { class: "sql-caption" },
      meta
        ? meta.columns.map((c) => `${c.name} ${c.type.toLowerCase()}${c.pk ? " ·pk" : ""}`).join(" · ")
        : ""),
    body,
  );
  await load();

  async function load(): Promise<void> {
    body.replaceChildren(el("div", { class: "empty" }, "loading…"));
    const d = await getJSON<RowsPage>(
      `/api/db/${state.db}/rows/${encodeURIComponent(name!)}?limit=${limit}&offset=${offset}`);
    const tbl = el("table", { class: "grid" },
      el("thead", null, el("tr", null, d.columns.map((c) => el("th", null, c)))),
      el("tbody", null, d.rows.map((r) =>
        el("tr", null, r.map((v) =>
          el("td", { class: typeof v === "number" ? "num" : null }, v === null ? "∅" : v))))));
    const pager = el("div", { class: "pager" },
      el("button", {
        class: "btn",
        disabled: offset === 0 ? "" : null,
        onclick: () => {
          offset = Math.max(0, offset - limit);
          load();
        },
      }, "‹ prev"),
      el("button", {
        class: "btn",
        disabled: offset + limit >= d.total ? "" : null,
        onclick: () => {
          offset += limit;
          load();
        },
      }, "next ›"),
      el("span", { class: "where" },
        `rows ${d.total ? offset + 1 : 0}–${Math.min(offset + limit, d.total)} of ${d.total.toLocaleString()}`));
    body.replaceChildren(el("div", { class: "table-scroll" }, tbl), pager);
  }
}

/* ---------- graph view (knowledge graph, schema layer) ---------- */

async function viewGraph(host: HTMLElement): Promise<void> {
  setCrumbs("graph");
  host.append(
    el("h1", { class: "h1" }, "knowledge graph"),
    el("p", { class: "lede" },
      "the schema layer: tables as entities, declared foreign keys as relations. the instance layer (records, aliases, cross-row links) arrives with the knowledge store."));
  const g = await getJSON<SchemaGraph>(`/api/db/${state.db}/graph`);
  if (!g.nodes.length) {
    host.append(el("div", { class: "empty" }, "— no tables"));
    return;
  }
  const W = 900, H = Math.max(420, 90 * Math.ceil(g.nodes.length / 2));
  const cx = W / 2, cy = H / 2, R = Math.min(cx, cy) - 90;
  const pos = new Map<string, { x: number; y: number }>();
  g.nodes.forEach((n, i) => {
    const a = (2 * Math.PI * i) / g.nodes.length - Math.PI / 2;
    pos.set(n.id, { x: cx + R * Math.cos(a), y: cy + R * Math.sin(a) });
  });
  const svg = svgEl("svg", {
    class: "graph-svg",
    viewBox: `0 0 ${W} ${H}`,
    role: "img",
    "aria-label": "schema graph",
  });
  svg.append(svgEl("defs", null,
    svgEl("marker", {
      id: "arrow", viewBox: "0 0 8 8", refX: 7, refY: 4,
      markerWidth: 6, markerHeight: 6, orient: "auto",
    }, svgEl("path", { d: "M 0 0 L 8 4 L 0 8 z", fill: "var(--flat)" }))));
  for (const e of g.edges) {
    const a = pos.get(e.source), b = pos.get(e.target);
    if (!a || !b) continue;
    const mx = (a.x + b.x) / 2 + (a.y - b.y) * 0.12, my = (a.y + b.y) / 2 + (b.x - a.x) * 0.12;
    svg.append(
      svgEl("path", {
        class: "gedge",
        d: `M ${a.x} ${a.y} Q ${mx} ${my} ${b.x} ${b.y}`,
        "marker-end": "url(#arrow)",
      }),
      svgEl("text", { class: "gedge-label", x: mx, y: my, "text-anchor": "middle" }, e.label));
  }
  for (const n of g.nodes) {
    const p = pos.get(n.id)!;
    const w = Math.max(90, n.id.length * 8 + 26);
    const grp = svgEl("g", {
      class: "gnode",
      transform: `translate(${p.x - w / 2}, ${p.y - 20})`,
      cursor: "pointer",
    },
      svgEl("rect", { width: w, height: 40, rx: 4 }),
      svgEl("text", { x: w / 2, y: 17, "text-anchor": "middle" }, n.id),
      svgEl("text", { class: "grows", x: w / 2, y: 31, "text-anchor": "middle" },
        `${n.rows.toLocaleString()} rows`));
    grp.addEventListener("click", () => {
      location.hash = "#/data/" + encodeURIComponent(n.id);
    });
    hov(grp, `<b>${esc(n.id)}</b><br>${n.columns.map(esc).join("<br>")}`);
    svg.append(grp);
  }
  host.append(el("div", { class: "panel" }, svg));
}

/* ---------- store view ---------- */

async function viewStore(host: HTMLElement): Promise<void> {
  setCrumbs("store");
  host.append(
    el("h1", { class: "h1" }, "store"),
    el("p", { class: "lede" },
      "the .stemmadb sidecar: every derived artifact, all disposable. the user database is attached read-only and never touched."));
  const m = await getJSON<StoreMeta>(`/api/db/${state.db}/store`);
  if (!m.exists) {
    host.append(el("div", { class: "empty" },
      "— no store yet; it is created when stemma-server registers the database"));
    return;
  }
  const kv = (pairs: [string, string | number][]) =>
    el("div", { class: "kv" },
      pairs.map(([k, v]) => [el("span", { class: "k" }, k), el("span", { class: "v" }, v)]));
  host.append(el("div", { class: "section panel" },
    el("div", { class: "subhead" }, "store file"),
    kv([
      ["path", m.path ?? ""],
      ["size", ((m.size_bytes ?? 0) / 1e6).toFixed(1) + " MB"],
      ["schema version", m.schema_version ?? 0],
    ])));
  host.append(el("div", { class: "section panel" },
    el("div", { class: "subhead" }, "lexical index"),
    m.lexical
      ? kv([
        ["values", m.lexical.values.toLocaleString()],
        ["tables", m.lexical.tables],
        ["indexed columns", m.lexical.columns],
        ["channels", "exact · bm25 · trigram"],
      ])
      : el("div", { class: "empty" }, "— not built; starts with stemma-server registration")));
  const reg = el("div", { class: "section panel" }, el("div", { class: "subhead" }, "model registry"));
  const registry = m.model_registry ?? [];
  if (registry.length) {
    reg.append(el("div", { class: "table-scroll" }, el("table", { class: "grid" },
      el("thead", null, el("tr", null, Object.keys(registry[0]).map((c) => el("th", null, c)))),
      el("tbody", null, registry.map((r) =>
        el("tr", null, Object.values(r).map((v) => el("td", null, v))))))));
  } else {
    reg.append(el("div", { class: "empty" },
      "— no vector tables yet · the dense channel lands in milestone 3"));
  }
  host.append(reg);
  host.append(el("div", { class: "section panel" },
    el("div", { class: "subhead" }, "embed queue"),
    kv([["pending", m.embed_queue ?? 0]])));
}

/* ---------- sql view ---------- */

function viewSql(host: HTMLElement): void {
  setCrumbs("sql");
  const box = el("textarea", { class: "input sqlbox mono", placeholder: "SELECT …" },
    "SELECT src_table, src_column, count(*) AS n\nFROM lex_values GROUP BY 1, 2 ORDER BY n DESC");
  const out = el("div", null);
  const run = async (): Promise<void> => {
    out.replaceChildren(el("div", { class: "empty" }, "running…"));
    try {
      const r = await fetch(`/api/db/${state.db}/sql`, {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ sql: box.value }),
      });
      const d = (await r.json()) as SqlResult & { detail?: string };
      if (!r.ok) throw new Error(d.detail ?? r.statusText);
      out.replaceChildren(
        el("div", { class: "sql-caption" },
          `${d.rows.length} row${d.rows.length === 1 ? "" : "s"}${d.truncated ? " (truncated)" : ""} · ${d.elapsed_ms} ms`),
        el("div", { class: "table-scroll" }, el("table", { class: "grid" },
          el("thead", null, el("tr", null, d.columns.map((c) => el("th", null, c)))),
          el("tbody", null, d.rows.map((row) =>
            el("tr", null, row.map((v) => el("td", null, v === null ? "∅" : v))))))));
    } catch (e) {
      out.replaceChildren(el("div", { class: "sql-error" }, (e as Error).message));
    }
  };
  box.addEventListener("keydown", (e) => {
    if ((e.ctrlKey || e.metaKey) && e.key === "Enter") run();
  });
  host.append(
    el("h1", { class: "h1" }, "sql"),
    el("div", { class: "sql-caption" },
      "read-only · main = the .stemmadb store · src = the user database · ctrl-enter runs"),
    box,
    el("div", { style: "margin-top:8px" },
      el("button", { class: "btn accent", onclick: run }, "run")),
    out,
  );
}

/* ---------- router ---------- */

async function route(): Promise<void> {
  const hash = location.hash || "#/resolve";
  const [path, qs] = hash.slice(2).split("?");
  const [view, arg] = path.split("/");
  const params = new URLSearchParams(qs ?? "");
  state.view = view || "resolve";

  document.querySelectorAll<HTMLAnchorElement>("#nav a").forEach((a) =>
    a.classList.toggle("on", a.dataset.view === state.view));

  if (!state.schema && state.db) {
    try {
      state.schema = await getJSON<Schema>(`/api/db/${state.db}/schema`);
    } catch (_e) {
      state.schema = { tables: [] };
    }
  }
  renderSidebar();

  const host = document.getElementById("view") as HTMLElement;
  host.replaceChildren();
  try {
    if (state.view === "data") await viewData(host, params, arg ? decodeURIComponent(arg) : undefined);
    else if (state.view === "graph") await viewGraph(host);
    else if (state.view === "store") await viewStore(host);
    else if (state.view === "sql") viewSql(host);
    else viewResolve(host, params);
  } catch (e) {
    host.append(el("div", { class: "sql-error" }, (e as Error).message));
  }
}

/* ---------- boot ---------- */

(async function boot() {
  initTheme();
  const cfg = await getJSON<{ databases: string[]; grpc: string }>("/api/config");
  state.dbs = cfg.databases;
  state.db = cfg.databases[0] ?? null;
  (document.getElementById("topsearch") as HTMLInputElement).addEventListener("keydown", (ev) => {
    const e = ev as KeyboardEvent;
    const target = ev.target as HTMLInputElement;
    if (e.key === "Enter" && target.value.trim()) {
      location.hash = "#/resolve?q=" + encodeURIComponent(target.value);
    }
  });
  globalThis.addEventListener("hashchange", route);
  pollHealth();
  route();
})();
