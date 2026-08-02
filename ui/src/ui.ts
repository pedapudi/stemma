/* stemma console — TypeScript, no framework, no npm.
 * Source of truth for ui/static/ui.js; build with ui/build.sh (deno). */

/* ---------- API payload types ---------- */

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
  snippet: string;
  is_doc: boolean;
  /* the LM adjudication band chose this candidate (reordered to front) */
  adjudicated?: boolean;
  /* verified join path to a co-mention's candidate, e.g.
     "people #2 ←lead_id— teams #43" */
  coherence?: string;
}

interface TraceSpan {
  id: number;
  text: string;
  start: number;
  end: number;
  status: string;
  candidates: TraceCandidate[];
  kg_alias: boolean;
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
  has_more: boolean;
}

interface GraphNode {
  key: string;
  kind: string;
  label: string;
  props: Record<string, unknown>;
}

interface GraphEdge {
  source: string;
  target: string;
  kind: string;
  label: string;
  props: Record<string, unknown>;
}

interface KnowledgeGraph {
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
  kg?: { layer: string; nodes: number; edges: number } | null;
}

interface PlanRow {
  id: number;
  parent: number;
  depth: number;
  detail: string;
}

interface SqlResult {
  columns: string[];
  rows: (string | number | null)[][];
  truncated: boolean;
  elapsed_ms: number;
  plan: PlanRow[];
  detail?: string;
}

interface Health {
  grpc: boolean;
  latency_ms: number;
}

interface ChatTrailItem {
  tool: string;
  args: Record<string, unknown>;
  result: unknown;
  trace?: Trace;
}

interface ChatResponse {
  message: string;
  trail: ChatTrailItem[];
}

interface Config {
  databases: string[];
  grpc: string;
  lm: { endpoint: string; model: string } | null;
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

/* Minimal markdown → DOM: headings, lists, bold/italic/code, fenced code,
 * http links. Hand-rolled (no npm), building nodes — never raw innerHTML. */
function md(text: string): HTMLElement {
  const root = el("div", { class: "md-root" });
  const blocks = text.split(/```/);
  blocks.forEach((block, bi) => {
    if (bi % 2 === 1) {
      root.append(el("pre", { class: "md-code" }, block.replace(/^\w*\n/, "")));
      return;
    }
    let list: HTMLElement | null = null;
    for (const rawLine of block.split("\n")) {
      const line = rawLine.trimEnd();
      if (!line.trim()) {
        list = null;
        continue;
      }
      const h = line.match(/^(#{1,4})\s+(.*)$/);
      const li = line.match(/^\s*(?:[-*]|\d+\.)\s+(.*)$/);
      if (h) {
        list = null;
        root.append(el("div", { class: `md-h md-h${h[1].length}` }, ...mdInline(h[2])));
      } else if (li) {
        if (!list) {
          list = el("ul", { class: "md-list" });
          root.append(list);
        }
        list.append(el("li", null, ...mdInline(li[1])));
      } else {
        list = null;
        root.append(el("p", { class: "md-p" }, ...mdInline(line)));
      }
    }
  });
  return root;
}

function mdInline(text: string): (Node | string)[] {
  const out: (Node | string)[] = [];
  // tokens: `code`  **bold**  *italic*  [label](http url)
  const re = /(`[^`]+`)|(\*\*[^*]+\*\*)|(\*[^*]+\*)|(\[[^\]]+\]\(https?:[^)]+\))/g;
  let last = 0;
  for (let m = re.exec(text); m; m = re.exec(text)) {
    if (m.index > last) out.push(text.slice(last, m.index));
    const t = m[0];
    if (m[1]) out.push(el("code", { class: "md-codespan" }, t.slice(1, -1)));
    else if (m[2]) out.push(el("b", null, t.slice(2, -2)));
    else if (m[3]) out.push(el("i", null, t.slice(1, -1)));
    else if (m[4]) {
      const mm = t.match(/^\[([^\]]+)\]\((https?:[^)]+)\)$/);
      if (mm) out.push(el("a", { href: mm[2], target: "_blank", rel: "noreferrer" }, mm[1]));
    }
    last = m.index + t.length;
  }
  if (last < text.length) out.push(text.slice(last));
  return out;
}

/* The structured candidate hovercard: reference, score meter, snippet with
 * accent hits, channel chips in their own hues, the reject verdict. */
function hovCandidate(c: TraceCandidate): string {
  const snip = esc((c.snippet || c.value).slice(0, 170))
    .replace(/⟨/g, '<b class="hc-hit">').replace(/⟩/g, "</b>");
  const chips = c.channels.map((ch) =>
    `<span class="hc-ch hc-ch-${esc(ch.channel)}">${esc(ch.channel)} · ${ch.raw.toFixed(1)}</span>`
  ).join("");
  return `
    <div class="hc-head">
      <span class="hc-ref">${esc(c.table)}.${esc(c.column)}</span>
      <span class="hc-rowid">#${c.rowid}</span>
      <span class="hc-score">${c.score.toFixed(2)}</span>
    </div>
    <div class="hc-meter"><i style="width:${Math.round(c.score * 100)}%"></i></div>
    <div class="hc-snip">${snip}</div>
    <div class="hc-chips">${chips}</div>` +
    (c.coherence
      ? `<div class="hc-coh">⬡ ${esc(c.coherence)}</div>` : "") +
    (c.adjudicated
      ? '<div class="hc-adj">⚖ adjudicated — the lm chose this among near-ties</div>' : "") +
    (c.selected
      ? '<div class="hc-verdict hc-ok">selected</div>'
      : `<div class="hc-verdict hc-no">${esc((c.reject_reason || "rejected").replace(/_/g, " "))}</div>`) +
    '<div class="hc-hint">click for the card</div>';
}

/* Render a snippet whose hits are marked with ⟨⟩. */
function snippetNode(snippet: string): HTMLElement {
  const out = el("span", { class: "snippet" });
  const parts = snippet.split(/⟨([^⟩]*)⟩/);
  parts.forEach((p, i) => {
    if (i % 2 === 1) out.append(el("span", { class: "hit" }, p));
    else if (p) out.append(p);
  });
  return out;
}

/* singleton hovercard */
const hovercard = document.getElementById("hovercard") as HTMLDivElement;

/* The card must never outlive its anchor: mouseleave doesn't fire when the
 * node is removed from the DOM (view swaps, redraws), so every render path
 * calls hideHover(). */
function hideHover(): void {
  hovercard.classList.remove("on");
}

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

/* ---------- state ---------- */

const state: {
  cfg: Config | null;
  dbs: string[];
  db: string | null;
  schema: Schema | null;
  view: string;
} = { cfg: null, dbs: [], db: null, schema: null, view: "query" };

/* chat transcripts keyed by db:conversation; conversations persist in the
 * store's chat_log and resume across sessions */
type ChatMsg = { role: "user" | "assistant"; content: string; trail?: ChatTrailItem[] };
const chatLog = new Map<string, ChatMsg[]>();

function activeConv(db: string): string {
  return localStorage.getItem(`stemma.conv.${db}`) ?? "default";
}

function setActiveConv(db: string, conv: string): void {
  localStorage.setItem(`stemma.conv.${db}`, conv);
}

/* Theme rows: id, display name, swatch strip (paper · panel · ink · good ·
 * bad · accent) — the strips can't read live tokens, so the six chips are the
 * only literal colors outside the theme blocks. Copied from the family. */
const COLOR_THEMES: [string, string, [string, string, string, string, string, string]][] = [
  ["monokai", "monokai", ["#1e1f1c", "#272822", "#f8f8f2", "#a6e22e", "#f92672", "#66d9ef"]],
  ["solarized-dark", "solarized dark", ["#04222B", "#0A2D38", "#93A1A1", "#8BB80E", "#E0483C", "#2AA198"]],
  ["solarized-light", "solarized light", ["#FDF6E3", "#FBF1D6", "#586E75", "#6B9B0B", "#DC322F", "#268BD2"]],
  ["google-light", "google light", ["#FFFFFF", "#F4F4F4", "#474A4E", "#34A853", "#EA4335", "#1B9CB8"]],
  ["google-dark", "google dark", ["#202124", "#2C2D30", "#FFFFFF", "#34A853", "#EA4335", "#24C1E0"]],
  ["lunaria-light", "lunaria light", ["#EBE4E1", "#E2DCD9", "#363434", "#497D46", "#783C1F", "#3778A9"]],
  ["lunaria-eclipse", "lunaria eclipse", ["#323F46", "#3B484F", "#DFE2ED", "#BEDBC1", "#BA9088", "#C8429F"]],
  ["belafonte-day", "belafonte day", ["#D5CCBA", "#CCC3B2", "#34292D", "#6E6A4E", "#BE100E", "#426A79"]],
  ["belafonte-night", "belafonte night", ["#20111B", "#271821", "#D5CCBA", "#A6A07A", "#D6403E", "#6F8E97"]],
  ["paper", "paper", ["#F2EEDE", "#E6E2D3", "#1A1A1A", "#216609", "#CC3E28", "#1E6FCC"]],
  ["zenburn", "zenburn", ["#3A3A3A", "#424241", "#DCDCCC", "#8FB28F", "#CC9393", "#8CD0D3"]],
  ["selenized-black", "selenized black", ["#181818", "#202020", "#DEDEDE", "#83C746", "#FF5E56", "#56D8C9"]],
  ["relaxed", "relaxed", ["#353A44", "#3D424B", "#F7F7F7", "#A0AC77", "#BC5653", "#7EAAC7"]],
  ["espresso", "espresso", ["#323232", "#3A3A3A", "#FFFFFF", "#A5C261", "#D25252", "#6C99BB"]],
  ["dracula", "dracula", ["#282A36", "#343746", "#F8F8F2", "#50FA7B", "#FF5555", "#BD93F9"]],
  ["ubuntu", "ubuntu", ["#300A24", "#3D1530", "#EEEEEC", "#8AE234", "#CC0000", "#34E2E2"]],
];

const MONO_GSM = '"Google Sans Mono","Noto Sans Mono",ui-monospace,monospace';
const SANS_GROTESK = '"Space Grotesk",system-ui,sans-serif';

interface TypeOption {
  id: string;
  label: string;
  group: string;
  head: string; // the option label renders in this stack — a true specimen
  body: string; // the sample line renders in this stack
}

const TYPE_OPTIONS: TypeOption[] = [
  { id: "T7", label: "Google Sans Mono", group: "technical", head: MONO_GSM, body: MONO_GSM },
  { id: "T9", label: "Source Sans 3 + Source Code Pro", group: "technical",
    head: '"Source Sans 3",system-ui,sans-serif', body: '"Source Sans 3",system-ui,sans-serif' },
  { id: "T12", label: "Inconsolata", group: "technical",
    head: '"Inconsolata",ui-monospace,monospace', body: '"Inconsolata",ui-monospace,monospace' },
  { id: "T14", label: "Ubuntu + Ubuntu Mono", group: "technical",
    head: '"Ubuntu",system-ui,sans-serif', body: '"Ubuntu",system-ui,sans-serif' },
  { id: "E5", label: "Fraunces", group: "editorial",
    head: '"Fraunces",Georgia,serif', body: '"Fraunces",Georgia,serif' },
  { id: "E7", label: "Bitter", group: "editorial",
    head: '"Bitter",Georgia,serif', body: '"Bitter",Georgia,serif' },
  { id: "E8", label: "Literata", group: "editorial",
    head: '"Literata",Georgia,serif', body: '"Literata",Georgia,serif' },
  { id: "E15", label: "Domine", group: "editorial",
    head: '"Domine",Georgia,serif', body: '"Domine",Georgia,serif' },
  { id: "D2", label: "Archivo Narrow + Space Grotesk", group: "display",
    head: `"Archivo Narrow",${SANS_GROTESK}`, body: SANS_GROTESK },
  { id: "D12", label: "Hanken Grotesk", group: "display",
    head: '"Hanken Grotesk",system-ui,sans-serif', body: '"Hanken Grotesk",system-ui,sans-serif' },
  { id: "D14", label: "Barlow Condensed + Space Grotesk", group: "display",
    head: '"Barlow Condensed","Archivo Narrow",system-ui,sans-serif', body: SANS_GROTESK },
  { id: "D5", label: "Bricolage Grotesque", group: "display",
    head: '"Bricolage Grotesque",system-ui,sans-serif', body: '"Bricolage Grotesque",system-ui,sans-serif' },
];
const SAMPLE_LINE = "the quick brown fox 0123";
const FONT_SIZES: [string, number][] = [["s", 1], ["m", 1.15], ["l", 1.3]];

/* ---------- chrome ---------- */

const openMenus: HTMLElement[] = [];

function closeAllMenus(except?: Element): void {
  for (const m of openMenus) {
    if (m !== except) m.classList.remove("open");
  }
}

function swatchStrip(colors: string[]): HTMLElement {
  const strip = el("span", { class: "swatch-strip", "aria-hidden": "true" });
  for (const c of colors) {
    const chip = el("i");
    chip.style.background = c;
    strip.append(chip);
  }
  return strip;
}

function buildThemePicker(): void {
  const mount = document.getElementById("themepicker") as HTMLElement;
  const saved = localStorage.getItem("stemma.theme") ?? "paper";
  document.documentElement.dataset.theme = saved;
  const current = COLOR_THEMES.find((t) => t[0] === saved) ?? COLOR_THEMES[9];

  const trigName = el("span", { class: "cd-name" }, current[1]);
  const trigStrip = el("span", null, swatchStrip(current[2]));
  const list = el("div", { class: "cd-list", role: "listbox" });
  const cd = el("span", { class: "cd" },
    el("button", {
      class: "cd-trigger",
      onclick: (e: Event) => {
        e.stopPropagation();
        hideHover();
        closeAllMenus(cd);
        cd.classList.toggle("open");
      },
    }, trigStrip, trigName, el("span", { class: "cd-caret" }, "▾")),
    list);

  for (const [id, label, colors] of COLOR_THEMES) {
    const opt = el("div", {
      class: "cd-option",
      role: "option",
      "aria-selected": id === saved ? "true" : "false",
      onclick: () => {
        document.documentElement.dataset.theme = id;
        localStorage.setItem("stemma.theme", id);
        list.querySelectorAll(".cd-option").forEach((o) => o.setAttribute("aria-selected", "false"));
        opt.setAttribute("aria-selected", "true");
        trigName.textContent = label;
        trigStrip.replaceChildren(swatchStrip(colors));
        cd.classList.remove("open");
      },
    }, swatchStrip(colors), el("span", { class: "cd-name" }, label));
    list.append(opt);
  }
  openMenus.push(cd);
  mount.append(cd);
}

function buildTypePicker(): void {
  const mount = document.getElementById("typepicker") as HTMLElement;
  const saved = localStorage.getItem("stemma.type") ?? "T9";
  if (saved !== "T9") document.documentElement.dataset.type = saved;
  const savedSize = localStorage.getItem("stemma.fontsize") ?? "s";
  const sizeVal = FONT_SIZES.find(([k]) => k === savedSize)?.[1] ?? 1;
  if (sizeVal !== 1) document.documentElement.style.setProperty("--fs", String(sizeVal));
  const current = TYPE_OPTIONS.find((o) => o.id === saved) ?? TYPE_OPTIONS[1];

  const trigName = el("span", { class: "cd-name" }, current.label);
  const trigSpec = el("span", { class: "tf-spec" }, "Ag");
  trigSpec.style.fontFamily = current.head;
  const listBox = el("div", { class: "tf-list" });
  const pop = el("div", { class: "tf-pop" }, listBox);
  const cd = el("span", { class: "cd" },
    el("button", {
      class: "cd-trigger",
      onclick: (e: Event) => {
        e.stopPropagation();
        hideHover();
        closeAllMenus(cd);
        cd.classList.toggle("open");
      },
    }, trigSpec, trigName, el("span", { class: "cd-caret" }, "▾")),
    pop);

  for (const group of ["technical", "editorial", "display"]) {
    listBox.append(el("div", { class: "tf-group" }, el("div", { class: "subhead" }, group)));
    for (const o of TYPE_OPTIONS.filter((x) => x.group === group)) {
      const name = el("span", { class: "tf-name" }, o.label);
      name.style.fontFamily = o.head;
      const sample = el("span", { class: "tf-sample", "aria-hidden": "true" }, SAMPLE_LINE);
      sample.style.fontFamily = o.body;
      const opt = el("div", {
        class: "tf-option",
        role: "option",
        "aria-selected": o.id === saved ? "true" : "false",
        onclick: () => {
          if (o.id === "T9") delete document.documentElement.dataset.type;
          else document.documentElement.dataset.type = o.id;
          localStorage.setItem("stemma.type", o.id);
          pop.querySelectorAll(".tf-option").forEach((x) => x.setAttribute("aria-selected", "false"));
          opt.setAttribute("aria-selected", "true");
          trigName.textContent = o.label;
          trigSpec.style.fontFamily = o.head;
        },
      }, name, sample);
      listBox.append(opt);
    }
  }

  // the S/M/L text-size footer
  const seg = el("span", { class: "seg" });
  for (const [k, v] of FONT_SIZES) {
    const b = el("button", {
      class: k === savedSize ? "on" : "",
      onclick: () => {
        document.documentElement.style.setProperty("--fs", String(v));
        localStorage.setItem("stemma.fontsize", k);
        seg.querySelectorAll("button").forEach((x) => x.classList.remove("on"));
        b.classList.add("on");
      },
    }, k);
    seg.append(b);
  }
  pop.append(el("div", { class: "tf-foot" }, el("span", { class: "k" }, "text size"), seg));

  openMenus.push(cd);
  mount.append(cd);
}

function initPickers(): void {
  buildThemePicker();
  buildTypePicker();
  document.addEventListener("click", () => closeAllMenus());
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
          if (chatRailOpen()) renderChatRail();
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
      el("span", { class: "count" }, "~" + t.row_count.toLocaleString())));
  }

  // the store, at a glance — replaces the former store tab
  const storeBox = el("div", { class: "side-store" },
    el("div", { class: "tree-group" }, "store"));
  side.append(storeBox);
  getJSON<StoreMeta>(`/api/db/${state.db}/store`).then((m) => {
    if (!m.exists) {
      storeBox.append(el("div", { class: "empty" }, "— not created yet"));
      return;
    }
    const pairs: [string, string][] = [
      ["size", ((m.size_bytes ?? 0) / 1e6).toFixed(1) + " mb"],
      ["lexical", m.lexical ? m.lexical.values.toLocaleString() + " values" : "—"],
      ["kg", m.kg ? `${m.kg.nodes} nodes · ${m.kg.edges} edges` : "—"],
      ["embed queue", String(m.embed_queue ?? 0)],
      ["vectors", (m.model_registry ?? []).length
        ? `${(m.model_registry ?? []).length} tables`
        : "none · m3"],
    ];
    storeBox.append(el("div", { class: "kv" },
      pairs.map(([k, v]) => [el("span", { class: "k" }, k), el("span", { class: "v" }, v)])));
  }).catch(() => storeBox.append(el("div", { class: "empty" }, "— unavailable")));
}

function setCrumbs(...parts: (string | undefined)[]): void {
  (document.getElementById("crumbs") as HTMLElement).textContent =
    [state.db, ...parts].filter(Boolean).join(" · ");
}

/* ---------- query view: natural language | sql ---------- */

function viewQuery(host: HTMLElement, params: URLSearchParams): void {
  const dialect = params.get("d") === "sql" ? "sql" : "nl";
  const q = params.get("q") ?? "";
  setCrumbs("query", dialect === "sql" ? "sql" : "natural");

  const seg = el("span", { class: "seg" },
    el("button", {
      class: dialect === "nl" ? "on" : "",
      onclick: () => {
        location.hash = "#/query?d=nl" + (q ? "&q=" + encodeURIComponent(q) : "");
      },
    }, "natural"),
    el("button", {
      class: dialect === "sql" ? "on" : "",
      onclick: () => {
        location.hash = "#/query?d=sql";
      },
    }, "sql"));

  host.append(
    el("div", { style: "display:flex; align-items:baseline; gap:14px" },
      el("h1", { class: "h1" }, "query"), seg),
    el("p", { class: "lede" },
      dialect === "nl"
        ? "the dialect is natural language: mentions resolve to records, and the trajectory shows every span considered, every channel fired, and every candidate — chosen and near-miss alike."
        : "the dialect is sql, read-only: main is the .stemmadb store, src is the user database. every query ships with its plan."),
  );

  if (dialect === "nl") queryNatural(host, q);
  else querySql(host);
}

function queryNatural(host: HTMLElement, q: string): void {
  const input = el("input", {
    class: "input",
    value: q,
    placeholder: "ask about the data — mentions resolve to records…",
    onkeydown: (e: Event) => {
      if ((e as KeyboardEvent).key === "Enter") run(input.value);
    },
  });
  const out = el("div", null);
  const examplesRow = el("div", null);
  host.append(
    el("div", { class: "queryrow" },
      input,
      el("button", { class: "btn accent", onclick: () => run(input.value) }, "resolve")),
    examplesRow,
    out,
  );

  // recent queries from the store's query_log
  getJSON<{ queries: string[] }>(`/api/db/${state.db}/history`).then((r) => {
    if (!r.queries.length) return;
    const row = el("div", null,
      el("span", { class: "sql-caption", style: "margin-right:8px" }, "recent:"));
    for (const x of r.queries.slice(0, 6)) {
      row.append(el("button", {
        class: "chip",
        style: "margin-right:6px; cursor:pointer",
        onclick: () => {
          input.value = x;
          run(x);
        },
      }, x.length > 60 ? x.slice(0, 60) + "…" : x));
    }
    examplesRow.after(row);
  }).catch(() => { /* history is a nicety */ });

  // examples mined from this database's knowledge graph
  getJSON<{ examples: string[] }>(`/api/db/${state.db}/examples`).then((r) => {
    for (const x of r.examples) {
      examplesRow.append(el("button", {
        class: "chip",
        style: "margin-right:6px; cursor:pointer",
        onclick: () => {
          input.value = x;
          run(x);
        },
      }, x));
    }
    if (r.examples.length) {
      examplesRow.prepend(el("span", {
        class: "sql-caption",
        style: "margin-right:8px",
      }, "from the kg:"));
    }
  }).catch(() => { /* examples are a nicety */ });

  if (q) run(q);

  async function run(query: string): Promise<void> {
    if (!query.trim()) return;
    history.replaceState(null, "", "#/query?d=nl&q=" + encodeURIComponent(query));
    (document.getElementById("topsearch") as HTMLInputElement).value = query;
    out.replaceChildren(el("div", { class: "empty" }, "resolving…"));
    let trace: Trace;
    if (pendingTrace && pendingTrace.query === query) {
      trace = pendingTrace;
      pendingTrace = null;
      renderTrace(out, trace);
      return;
    }
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

function querySql(host: HTMLElement): void {
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
        renderPlan(d.plan),
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
    box,
    el("div", { style: "margin-top:8px" },
      el("button", { class: "btn accent", onclick: run }, "run"),
      el("span", { class: "sql-caption", style: "margin-left:10px" }, "ctrl-enter runs")),
    out,
  );
}

/* the query-plan tree: SCAN in caution, SEARCH in good — direction earned */
function renderPlan(plan: PlanRow[]): HTMLElement {
  const box = el("div", { class: "plan panel" },
    el("div", { class: "subhead" }, "query plan"));
  if (!plan.length) {
    box.append(el("div", { class: "empty" }, "— trivial plan"));
    return box;
  }
  for (const p of plan) {
    const opClass = /^SCAN/.test(p.detail) ? "scan" : /^SEARCH/.test(p.detail) ? "search" : "";
    box.append(el("div", { class: "plan-row" },
      el("span", { class: "tick" }, "│ ".repeat(p.depth) + "├─"),
      el("span", { class: `op ${opClass}` }, p.detail)));
  }
  return box;
}

/* ---------- the trajectory ---------- */

function renderTrace(out: HTMLElement, trace: Trace): void {
  out.replaceChildren();
  hideHover();
  const mentionSpans = trace.mentions.map((i) => trace.spans[i]);

  /* one stable hue per source table, shared by every view in this
   * trajectory (dots, lanes, lattice fills, mention chips). --accent is
   * reserved for "selected", so tables draw from the rest of the palette. */
  const TABLE_HUES = [
    "var(--caution)", "var(--brand-accent)", "var(--bad)", "var(--good)", "var(--flat)",
  ];
  const tablesSeen = [...new Set(
    trace.spans.flatMap((s) => s.candidates.map((c) => c.table)))].sort();
  const hueOf = (t: string) =>
    TABLE_HUES[tablesSeen.indexOf(t) % TABLE_HUES.length];

  /* the lineage: the query line above, the substituted form below — every
   * mention replaced by the entity it resolves to — with wires tying each
   * (sub)string to its replacement. */
  const qline = el("div", { class: "qline" });
  const covered = (pos: number) => mentionSpans.find((s) => pos >= s.start && pos < s.end);
  const spanTok = new Map<number, HTMLElement>();
  let cursor = 0;
  for (const t of trace.tokens) {
    if (t.start > cursor) qline.append(trace.query.slice(cursor, t.start));
    const m = covered(t.start);
    const node = el("span", {
      class: "qtok" + (m ? " mention" : t.stopword ? " stop" : ""),
    }, t.text);
    if (m && !spanTok.has(m.id)) spanTok.set(m.id, node);
    qline.append(node);
    cursor = t.end;
  }
  if (cursor < trace.query.length) qline.append(trace.query.slice(cursor));

  const subline = el("div", { class: "subline" });
  const spanChip = new Map<number, HTMLElement>();
  cursor = 0;
  const emitPlain = (from: number, to: number) => {
    if (to > from) subline.append(el("span", { class: "sub-plain" }, trace.query.slice(from, to)));
  };
  for (const sp of mentionSpans) {
    emitPlain(cursor, sp.start);
    const top = sp.candidates.find((c) => c.selected);
    let chip: HTMLElement;
    if (top) {
      const label = top.is_doc
        ? `${top.table} #${top.rowid}`
        : `\u201c${top.value.length > 28 ? top.value.slice(0, 28) + "\u2026" : top.value}\u201d`;
      chip = el("button", {
        class: "sub-chip",
        title: `${top.table}.${top.column} #${top.rowid}`,
        onclick: (e: Event) => {
          e.stopPropagation();
          showCard(sp, top);
        },
      },
        tablesSeen.length > 1
          ? el("i", { class: "chip-dot", style: `background:${hueOf(top.table)}` })
          : null,
        label);
      hov(chip, hovCandidate(top));
    } else {
      chip = el("span", { class: "sub-chip sub-unresolved", title: "unresolved" }, sp.text);
    }
    spanChip.set(sp.id, chip);
    subline.append(chip);
    cursor = sp.end;
  }
  emitPlain(cursor, trace.query.length);

  const wires = svgEl("svg", { class: "traj-wires", "aria-hidden": "true" });
  const lineage = el("div", { class: "lineage" }, wires, qline, subline);

  function drawLineage(): void {
    wires.replaceChildren();
    const box = lineage.getBoundingClientRect();
    if (!box.width) return;
    wires.setAttribute("viewBox", `0 0 ${box.width} ${box.height}`);
    for (const sp of mentionSpans) {
      const a = spanTok.get(sp.id), b = spanChip.get(sp.id);
      if (!a || !b) continue;
      const ra = a.getBoundingClientRect(), rb = b.getBoundingClientRect();
      const x1 = ra.left - box.left + ra.width / 2, y1 = ra.bottom - box.top + 1;
      const x2 = rb.left - box.left + rb.width / 2, y2 = rb.top - box.top - 1;
      const resolved = !b.classList.contains("sub-unresolved");
      wires.append(svgEl("path", {
        class: "lineage-wire" + (resolved ? "" : " lost"),
        d: `M ${x1} ${y1} C ${x1} ${(y1 + y2) / 2}, ${x2} ${(y1 + y2) / 2}, ${x2} ${y2}`,
      }));
    }
  }
  requestAnimationFrame(drawLineage);

  /* the candidate card: opened by any view (lattice bars, verdict refs,
   * spectrum dots) for one candidate's full detail */
  const card = el("div", { class: "cand-card", hidden: "" });

  function showCard(sp: TraceSpan, c: TraceCandidate): void {
    hideHover();
    card.hidden = false;
    card.replaceChildren(
      el("div", { class: "cc-head" },
        el("span", { class: "cand-id" }, `${c.table}.${c.column} `,
          el("span", { class: "rowid" }, `#${c.rowid}`)),
        el("span", { class: "score" }, c.score.toFixed(3)),
        c.selected ? el("span", { class: "pill pending" }, "selected")
          : el("span", { class: "pill bad" }, (c.reject_reason || "rejected").replace(/_/g, " ")),
        el("span", { class: "spacer" }),
        el("button", { class: "btn", onclick: () => { card.hidden = true; } }, "✕")),
      el("div", { class: "cc-body" },
        c.is_doc && c.snippet ? snippetNode(c.snippet) : el("span", { class: "mono" }, `“${c.value}”`)),
      el("div", { class: "cc-channels" },
        c.channels.map((ch) =>
          el("span", { class: "chip" }, `${ch.channel} · rank ${ch.rank + 1} · ${ch.raw.toFixed(2)}`))),
      el("div", { class: "cc-actions" },
        el("button", {
          class: "btn accent",
          onclick: () => {
            location.hash = `#/data/${encodeURIComponent(c.table)}?after=${Number(c.rowid) - 1}`;
          },
        }, "open row in data →"),
        c.is_doc ? null : el("button", {
          class: "btn",
          onclick: () => {
            location.hash = "#/query?d=nl&q=" + encodeURIComponent(c.value);
          },
        }, "resolve this value →"),
        el("span", { class: "sql-caption" }, `for mention “${sp.text}”`)),
    );
  }

  /* the near-miss ledger, folded by default */
  const also = trace.spans
    .filter((s) => s.status !== "selected" && s.status !== "skipped")
    .sort((a, b) => a.start - b.start);
  const alsoBox = el("details", { class: "alsoran section" },
    el("summary", { class: "subhead", style: "cursor:pointer; display:inline-block" },
      `spans considered · ${also.length}`));
  const statusPill: Record<string, [string, string]> = {
    overlapped: ["neutral", "overlapped"],
    weak: ["caution", "weak"],
    no_candidates: ["neutral", "no match"],
  };
  for (const s of also) {
    const [tone, label] = statusPill[s.status] ?? ["neutral", s.status];
    const row = el("div", { class: "alsoran-row" },
      el("span", { class: "spantext" }, `\u201c${s.text}\u201d`),
      el("span", { class: "pill " + tone }, label),
      el("span", { class: "alsoran-cands" },
        s.candidates.length
          ? s.candidates.map((c) =>
            `${c.table}.${c.column} #${c.rowid} (${c.score.toFixed(2)})`).join(" · ")
          : "—"));
    if (s.candidates.length) {
      hov(row, s.candidates.slice(0, 3).map(hovCandidate).join("<hr>"));
    }
    alsoBox.append(row);
  }
  if (!also.length) {
    alsoBox.append(el("div", { class: "empty" }, "— every considered span became a mention"));
  }

  /* coherence ribbons: mentions whose candidates verified a join path get an
   * arc below the substituted line, the path as its label — the collective
   * layer made visible. */
  const cohPairs: { a: number; b: number; label: string }[] = [];
  for (let i = 0; i < mentionSpans.length; i++) {
    for (let j = i + 1; j < mentionSpans.length; j++) {
      const ca = mentionSpans[i].candidates.find((c) => c.selected && c.coherence);
      const cb = mentionSpans[j].candidates.find((c) => c.selected && c.coherence);
      if (ca && cb && ca.coherence === cb.coherence) {
        cohPairs.push({ a: mentionSpans[i].id, b: mentionSpans[j].id, label: ca.coherence as string });
      }
    }
  }
  if (cohPairs.length) lineage.classList.add("has-coh");

  function drawCoherence(): void {
    const box = lineage.getBoundingClientRect();
    for (const p of cohPairs) {
      const a = spanChip.get(p.a), b = spanChip.get(p.b);
      if (!a || !b) continue;
      const ra = a.getBoundingClientRect(), rb = b.getBoundingClientRect();
      const x1 = ra.left - box.left + ra.width / 2, x2 = rb.left - box.left + rb.width / 2;
      const y = Math.max(ra.bottom, rb.bottom) - box.top + 2;
      const dip = y + 16;
      wires.append(svgEl("path", {
        class: "coh-arc",
        d: `M ${x1} ${y} C ${x1} ${dip}, ${x2} ${dip}, ${x2} ${y}`,
      }));
      const t = svgEl("text", {
        class: "coh-label",
        x: (x1 + x2) / 2,
        y: dip + 12,
        "text-anchor": "middle",
      });
      t.textContent = `⬡ ${p.label}`;
      wires.append(t);
    }
  }
  requestAnimationFrame(drawCoherence);

  /* ---- the analytical panels: anatomy (span lattice + verdicts), space
   * (the dense channel's cosine spectrum) ---- */
  const panels: Record<string, { node: HTMLElement; built: boolean; build: () => void }> = {
    anatomy: { node: el("div", { class: "anatomy" }), built: false, build: buildAnatomy },
    space: { node: el("div", { class: "space" }), built: false, build: buildSpace },
  };
  const MODE_KEY = "stemma.trajmode";
  let mode = localStorage.getItem(MODE_KEY) || "anatomy";
  if (!(mode in panels)) mode = "anatomy";
  const modeBtns = new Map<string, HTMLElement>();
  const modeBar = el("div", { class: "traj-modes" },
    el("span", { class: "sql-caption", style: "margin-right:6px" }, "view:"),
    ...Object.keys(panels).map((m) => {
      const b = el("button", {
        class: "chip" + (m === mode ? " on-chan" : ""),
        onclick: () => setMode(m),
      }, m);
      modeBtns.set(m, b);
      return b;
    }),
    // when more than one table answered, its hue is the shared legend
    ...(tablesSeen.length > 1
      ? [el("span", { class: "spacer", style: "max-width:18px" }),
        ...tablesSeen.map((t) =>
          el("span", { class: "tbl-key mono" },
            el("i", { style: `background:${hueOf(t)}` }), t))]
      : []));

  function setMode(m: string): void {
    mode = m;
    localStorage.setItem(MODE_KEY, m);
    hideHover();
    for (const [k, p] of Object.entries(panels)) {
      if (k === m && !p.built) { p.build(); p.built = true; }
      p.node.hidden = k !== m;
      modeBtns.get(k)?.classList.toggle("on-chan", k === m);
    }
  }

  /* byte offsets (proto) → char offsets (layout): built once per trace */
  const b2c = (() => {
    const m = new Map<number, number>([[0, 0]]);
    let b = 0;
    const enc = new TextEncoder();
    [...trace.query].forEach((ch, i) => {
      b += enc.encode(ch).length;
      m.set(b, i + 1);
    });
    return (byte: number) => m.get(byte) ?? byte;
  })();

  /* -- anatomy · the span lattice: every enumerated span as a bar over the
   * query's character grid, stacked by n-gram width, status made visible —
   * the segmentation decision at a glance. -- */
  function buildAnatomy(): void {
    const host = panels.anatomy.node;
    const tokensIn = (sp: TraceSpan) =>
      trace.tokens.filter((t) => t.start >= sp.start && t.end <= sp.end).length;

    host.append(el("div", { class: "subhead" }, "span lattice"),
      el("div", { class: "sql-caption" },
        "every span the pipeline enumerated · winners tile the query, the rest lost their range"));

    const lat = el("div", { class: "lattice mono" });
    lat.append(el("div", { class: "lat-q" }, trace.query));

    const byLen = new Map<number, TraceSpan[]>();
    for (const sp of trace.spans) {
      const n = tokensIn(sp);
      if (!byLen.has(n)) byLen.set(n, []);
      (byLen.get(n) as TraceSpan[]).push(sp);
    }
    const lens = [...byLen.keys()].sort((a, b) => b - a);
    for (const n of lens) {
      const spans = (byLen.get(n) as TraceSpan[]).sort((a, b) => a.start - b.start);
      // greedy lane assignment: same-width n-grams overlap their neighbors
      const laneEnds: number[] = [];
      const lanes = spans.map((sp) => {
        const i = laneEnds.findIndex((e) => e <= sp.start);
        const lane = i === -1 ? laneEnds.length : i;
        laneEnds[lane] = sp.end;
        return lane;
      });
      const track = el("div", {
        class: "lat-track",
        style: `height:${(Math.max(...lanes) + 1) * 13}px`,
      });
      spans.forEach((sp, k) => {
        const top = sp.candidates[0];
        const isMention = trace.mentions.includes(sp.id);
        const cls = "lat-bar lat-" + (isMention ? "won" : sp.status);
        const bar = el("button", {
          class: cls,
          style: `left:${b2c(sp.start)}ch; width:${Math.max(1, b2c(sp.end) - b2c(sp.start))}ch; top:${lanes[k] * 13}px;` +
            (top && sp.status !== "skipped"
              ? `--w:${Math.round(Math.min(1, top.score) * 100)}%; --th:${hueOf(top.table)}` : ""),
          onclick: top ? () => showCard(sp, top) : null,
        });
        if (top) bar.append(el("i", { class: "lat-fill" }));
        hov(bar, top
          ? `<div class="hc-head"><span class="hc-ref">“${esc(sp.text)}”</span>` +
            `<span class="hc-score">${top.score.toFixed(2)}</span></div>` + hovCandidate(top)
          : `<div class="hc-head"><span class="hc-ref">“${esc(sp.text)}”</span></div>` +
            `<div class="hc-verdict hc-no">${esc(sp.status.replace(/_/g, " "))}</div>`);
        track.append(bar);
      });
      lat.append(el("div", { class: "lat-row" },
        el("span", { class: "lat-lab" }, n > MAX_LAT_N ? "whole" : String(n)),
        track));
    }
    lat.append(el("div", { class: "lat-legend sql-caption" },
      el("i", { class: "lat-key lat-won" }), " mention · ",
      el("i", { class: "lat-key lat-overlapped" }), " overlapped · ",
      el("i", { class: "lat-key lat-weak" }), " weak · ",
      el("i", { class: "lat-key lat-no_candidates" }), " no match · ",
      el("i", { class: "lat-key lat-skipped" }), " skipped"));
    host.append(lat);

    /* -- verdicts: why each winner won, narrated from the trace — the
     * channels that fired, the mechanism that decided it, and who was
     * beaten and why. Typography over abstraction. -- */
    host.append(el("div", { class: "subhead", style: "margin-top:18px" }, "verdicts"),
      el("div", { class: "sql-caption" },
        "per mention: what the evidence was, which mechanism decided it, and who lost"));
    for (const sp of mentionSpans) host.append(verdict(sp));
    if (!mentionSpans.length) {
      host.append(el("div", { class: "empty" }, "— no mentions, no verdicts"));
    }
  }

  /* the scoring stages, recomputed client-side from the trace (mirrors
   * fuse(): rrf base, branch envelope, calibrated cosine floor) */
  function stages(sp: TraceSpan, c: TraceCandidate) {
    const spanChars = [...sp.text].length;
    const nonKg = c.channels.filter((ch) => ch.channel !== "kg");
    const rrf = nonKg.reduce((s, ch) => s + 1 / (4 + ch.rank), 0);
    const base = Math.min((rrf * 4) / 3, 1);
    const hasExact = c.channels.some((ch) => ch.channel === "exact");
    const valChars = Math.max(1, [...c.value].length + (c.value_truncated ? 40 : 0));
    const branch = hasExact
      ? Math.min(0.9 + 0.1 * base, 1)
      : c.is_doc
        ? Math.min(base * 0.85, 0.85)
        : base * (0.4 + 0.6 * Math.sqrt(spanChars / Math.max(valChars, spanChars)));
    const cos = c.channels.filter((ch) => ch.channel === "dense")
      .reduce((m, ch) => Math.max(m, ch.raw), -1);
    const calibrated = cos >= 0
      ? Math.min(Math.max((cos - 0.3) / 0.3, 0), 1) * 0.78 : 0;
    return { base, branch, cos, calibrated, hasExact };
  }

  function verdict(sp: TraceSpan): HTMLElement {
    const w = sp.candidates.find((c) => c.selected) ?? sp.candidates[0];
    const box = el("div", { class: "why" });
    if (!w) {
      box.append(el("div", { class: "why-head" },
        el("span", { class: "sf-mention" }, sp.text),
        el("span", { class: "sql-caption" }, " — unresolved")));
      return box;
    }
    const s = stages(sp, w);
    // the deciding mechanism gets a hue: exact = good, semantic floor =
    // the dense hue, plain lexical = flat — carried by a dot, never a rail
    const mechHue = s.hasExact
      ? "var(--good)"
      : s.calibrated > s.branch + 0.005
        ? "color-mix(in srgb, var(--brand-accent) 60%, var(--ink))"
        : "var(--flat)";
    const meter = el("span", { class: "why-meter" },
      el("i", { style: `width:${Math.round(Math.min(1, w.score) * 100)}%` }));
    const head = el("div", { class: "why-head" },
      el("span", { class: "sf-mention" }, sp.text),
      el("span", { class: "why-arrow" }, "→"),
      el("button", {
        class: "why-ref mono", style: `color:${hueOf(w.table)}`,
        onclick: () => showCard(sp, w),
      }, `${w.table}.${w.column} #${w.rowid}`),
      meter,
      el("span", { class: "why-score mono" }, w.score.toFixed(2)));
    box.append(head);
    if (w.is_doc && w.snippet) {
      box.append(el("div", { class: "why-snip" }, snippetNode(w.snippet)));
    } else if (!w.is_doc) {
      box.append(el("div", { class: "why-snip mono" }, `“${w.value}”`));
    }

    // evidence: the channels that fired, in their own hues
    box.append(el("div", { class: "why-line" },
      el("span", { class: "why-k" }, "evidence"),
      el("span", { class: "hc-chips" },
        w.channels.map((ch) => {
          const chip = el("span", { class: `hc-ch hc-ch-${ch.channel}` },
            ch.channel === "dense" ? `dense · cos ${ch.raw.toFixed(2)}`
              : ch.channel === "kg" ? `kg +${ch.raw.toFixed(2)}`
                : `${ch.channel} · rank ${ch.rank + 1}`);
          return chip;
        }))));

    // the mechanism that decided it
    const mechDot = () => el("i", { class: "why-dot", style: `background:${mechHue}` });
    const mech: HTMLElement[] = [];
    if (s.hasExact) {
      mech.push(el("div", { class: "why-line" },
        el("span", { class: "why-k" }, "decided by"), mechDot(),
        el("span", null, "exact match — the mention equals the stored value, floor 0.9")));
    } else if (s.calibrated > s.branch + 0.005) {
      mech.push(el("div", { class: "why-line" },
        el("span", { class: "why-k" }, "decided by"), mechDot(),
        el("span", null,
          `semantic floor — cos ${s.cos.toFixed(2)} calibrates to ${s.calibrated.toFixed(2)}, above the lexical case (${s.branch.toFixed(2)})`)));
    } else {
      mech.push(el("div", { class: "why-line" },
        el("span", { class: "why-k" }, "decided by"), mechDot(),
        el("span", null, w.is_doc
          ? "fused lexical evidence under document scoring (length is not held against it)"
          : "fused lexical evidence with length affinity")));
    }
    if (w.coherence) {
      mech.push(el("div", { class: "why-line why-coh" },
        el("span", { class: "why-k" }, "coherence"),
        el("span", { class: "mono" }, `⬡ ${w.coherence} `),
        el("span", { class: "why-soft" }, "— verified in the data, +0.15")));
    }
    if (w.adjudicated) {
      mech.push(el("div", { class: "why-line why-adj" },
        el("span", { class: "why-k" }, "adjudicated"),
        el("span", null, "⚖ the lm chose this among near-ties (gap < 0.08)")));
    }
    box.append(...mech);

    // who lost, and why
    const r = sp.candidates.filter((c) => c !== w)[0];
    if (r) {
      const rs = stages(sp, r);
      let why: string;
      if (w.coherence && !r.coherence) why = "no verified join path";
      else if (s.hasExact && !rs.hasExact) why = "no exact match";
      else if (s.calibrated > s.branch && rs.cos < s.cos) {
        why = rs.cos >= 0 ? `weaker semantic match (cos ${rs.cos.toFixed(2)})` : "no dense evidence";
      } else if (r.channels.length < w.channels.length) why = "fewer channels agreed";
      else why = "lower fused evidence";
      const rlabel = r.is_doc ? `${r.table} #${r.rowid}` : `“${r.value.slice(0, 32)}”`;
      const rrow = el("div", { class: "why-line why-rival" },
        el("span", { class: "why-k" }, "beat"),
        el("button", { class: "why-ref mono", onclick: () => showCard(sp, r) },
          `${rlabel} · ${r.score.toFixed(2)}`),
        el("span", { class: "why-soft" }, ` — ${why}`));
      hov(rrow, hovCandidate(r));
      box.append(rrow);
    } else {
      box.append(el("div", { class: "why-line why-rival" },
        el("span", { class: "why-k" }, "beat"),
        el("span", { class: "why-soft" }, "no rival — the only candidate")));
    }
    return box;
  }

  /* -- space · the dense channel's neighborhood: the query at the center,
   * every dense-retrieved record at its true cosine distance, grouped by
   * table. The vector space, honestly drawn. -- */
  function buildSpace(): void {
    const host = panels.space.node;
    type Hit = { c: TraceCandidate; cos: number; sp: TraceSpan };
    const best = new Map<string, Hit>();
    for (const sp of trace.spans) {
      for (const c of sp.candidates) {
        const cos = c.channels.filter((ch) => ch.channel === "dense")
          .reduce((m, ch) => Math.max(m, ch.raw), -1);
        if (cos < 0) continue;
        const k = `${c.table}#${c.rowid}`;
        const prev = best.get(k);
        if (!prev || cos > prev.cos || (c.selected && !prev.c.selected)) {
          best.set(k, { c, cos, sp });
        }
      }
    }
    const hits = [...best.values()].sort((a, b) => b.cos - a.cos);
    host.append(el("div", { class: "subhead" }, "semantic spectrum"),
      el("div", { class: "sql-caption" },
        "every dense-retrieved record on the cosine axis · right is nearer the query · one lane per table"));
    if (!hits.length) {
      host.append(el("div", { class: "empty" },
        "— no dense evidence in this trajectory: the embedder was absent, or every span had exact lexical anchors"));
      return;
    }

    // cosine → x%, over the observed working range of the encoder
    const LO = 0.26, HI = 0.74;
    const xOf = (cos: number) =>
      Math.max(1.5, Math.min(98.5, ((cos - LO) / (HI - LO)) * 100));

    const spec = el("div", { class: "spec" });
    // the reference grid: calibration lines every lane shares
    const marks: [number, string][] = [
      [0.30, "0.30 · calibration floor"], [0.40, "0.40"],
      [0.50, "0.50"], [0.60, "0.60 · strong"],
    ];
    const grid = el("div", { class: "spec-grid" });
    for (const [cos, label] of marks) {
      grid.append(el("i", { class: "spec-rule", style: `left:${xOf(cos)}%` }));
      grid.append(el("span", { class: "spec-rulelab", style: `left:${xOf(cos)}%` }, label));
    }
    spec.append(grid);

    const tables = [...new Set(hits.map((h) => h.c.table))]
      .sort((a, b) => hits.filter((h) => h.c.table === b).length -
        hits.filter((h) => h.c.table === a).length);
    for (const t of tables) {
      const mine = hits.filter((h) => h.c.table === t);
      const lane = el("div", { class: "spec-lane" });
      for (const [cos] of marks) {
        lane.append(el("i", { class: "spec-rule", style: `left:${xOf(cos)}%` }));
      }
      // stack colliding dots downward instead of overplotting
      const placed: number[] = [];
      for (const h of mine) {
        const x = xOf(h.cos);
        const row = placed.filter((p) => Math.abs(p - x) < 2.2).length;
        placed.push(x);
        const dot = el("button", {
          class: "spec-dot" + (h.c.selected ? " sel" : ""),
          style: `left:${x}%; top:${8 + Math.min(row, 3) * 9}px; --th:${hueOf(h.c.table)}`,
        });
        hov(dot,
          `<div class="hc-head"><span class="hc-ref">for “${esc(h.sp.text)}”</span>` +
          `<span class="hc-score">cos ${h.cos.toFixed(3)}</span></div>` + hovCandidate(h.c));
        dot.addEventListener("click", (e: Event) => {
          e.stopPropagation();
          showCard(h.sp, h.c);
        });
        lane.append(dot);
      }
      const bestMine = mine[0];
      spec.append(el("div", { class: "spec-row" },
        el("span", { class: "spec-lab" },
          el("b", { style: `color:${hueOf(t)}` }, t),
          el("i", null, `${mine.length} · best ${bestMine.cos.toFixed(2)}`)),
        lane));
    }
    spec.append(el("div", { class: "spec-axis" },
      el("span", { class: "spec-end" }, "← noise"),
      el("span", { class: "spec-end spec-near" }, "nearer the query →")));
    host.append(spec,
      el("div", { class: "sql-caption", style: "margin-top:6px" },
        "accent dots were selected as mentions · grey dots are the retrieved-but-outranked field · click any dot for its card"));
  }

  for (const [k, p] of Object.entries(panels)) p.node.hidden = k !== mode;
  if (!panels[mode].built) {
    panels[mode].build();
    panels[mode].built = true;
  }

  out.append(
    el("div", { class: "sql-caption" },
      `resolved in ${trace.elapsed_ms.toFixed(1)} ms · ${trace.spans.length} spans enumerated · channels: exact, bm25, trigram, dense, kg`),
    lineage,
    modeBar,
    panels.anatomy.node,
    panels.space.node,
    card,
    alsoBox,
  );
}

/* n-grams wider than the enumeration cap are the whole-query span */
const MAX_LAT_N = 4;

/* The compact in-conversation trajectory: query line with mentions marked,
 * then each mention's selected candidates with meters — the same story as
 * the full view at rail width. */
function renderMiniTrace(trace: Trace): HTMLElement {
  const box = el("div", { class: "minitraj" });
  const mentionSpans = trace.mentions.map((i) => trace.spans[i]);
  const covered = (pos: number) => mentionSpans.some((s) => pos >= s.start && pos < s.end);
  const qline = el("div", { class: "mini-qline" });
  let cursor = 0;
  for (const t of trace.tokens) {
    if (t.start > cursor) qline.append(trace.query.slice(cursor, t.start));
    qline.append(el("span", {
      class: "qtok" + (covered(t.start) ? " mention" : t.stopword ? " stop" : ""),
    }, t.text));
    cursor = t.end;
  }
  if (cursor < trace.query.length) qline.append(trace.query.slice(cursor));
  box.append(qline);

  for (const sp of mentionSpans) {
    const sel = sp.candidates.filter((c) => c.selected);
    const missed = sp.candidates.length - sel.length;
    const lane = el("div", { class: "mini-lane" },
      el("div", { class: "mini-span" }, sp.text));
    for (const c of sel.slice(0, 3)) {
      lane.append(el("div", { class: "mini-cand" },
        el("span", { class: "mini-ref" }, `${c.table}.${c.column} #${c.rowid}`),
        c.is_doc && c.snippet
          ? snippetNode(c.snippet)
          : el("span", { class: "mini-val" }, `“${c.value}”`),
        el("span", { class: "meter" },
          el("span", { style: `width:${Math.round(c.score * 100)}%` })),
      ));
    }
    if (sel.length > 3 || missed > 0) {
      lane.append(el("div", { class: "mini-more" },
        [sel.length > 3 ? `+${sel.length - 3} more` : "",
          missed > 0 ? `${missed} near-miss${missed === 1 ? "" : "es"}` : ""]
          .filter(Boolean).join(" · ")));
    }
    box.append(lane);
  }
  if (!mentionSpans.length) {
    box.append(el("div", { class: "empty" }, "— nothing resolved"));
  }
  return box;
}

/* ---------- the chat rail ----------
 * Chat lives beside the work, not instead of it: a right-hand rail (the
 * copilot idiom from the family). When the model resolves a mention, the
 * trajectory can be pushed into the main query view — chat drives the
 * visual, it doesn't replace it. */

/* a trace handed from the rail (or elsewhere) to the query view, consumed
 * once instead of refetching */
let pendingTrace: Trace | null = null;

function showTraceInMain(trace: Trace): void {
  pendingTrace = trace;
  const target = "#/query?d=nl&q=" + encodeURIComponent(trace.query);
  if (location.hash === target) route();
  else location.hash = target;
}

function chatRailOpen(): boolean {
  return localStorage.getItem("stemma.chatrail") === "open";
}

function setChatRail(open: boolean): void {
  localStorage.setItem("stemma.chatrail", open ? "open" : "closed");
  const rail = document.getElementById("chatrail") as HTMLElement;
  const grid = document.getElementById("bodygrid") as HTMLElement;
  const btn = document.getElementById("chattoggle") as HTMLButtonElement;
  const savedW = localStorage.getItem("stemma.railw");
  if (savedW) grid.style.setProperty("--railw", savedW);
  rail.hidden = !open;
  grid.classList.toggle("chat-open", open);
  btn.classList.toggle("accent", open);
  hideHover();
  if (open) renderChatRail();
}

/* ---- tool-result rendering for the chat rail: structured views per tool,
 * never raw JSON. MCP results arrive enveloped ({content:[{type:"text",
 * text:"<json>"}], structuredContent?}) and often double-encoded; unwrap
 * before dispatching. ---- */
function unwrapToolResult(result: unknown): unknown {
  if (result == null || typeof result !== "object") {
    if (typeof result === "string") {
      try { return JSON.parse(result); } catch { return result; }
    }
    return result;
  }
  const r = result as Record<string, unknown>;
  const sc = r.structuredContent as Record<string, unknown> | undefined;
  if (sc) return sc.result ?? sc;
  const content = r.content as { type?: string; text?: string }[] | undefined;
  if (Array.isArray(content)) {
    const text = content.find((c) => c.type === "text")?.text;
    if (text != null) {
      try { return JSON.parse(text); } catch { return text; }
    }
  }
  return result;
}

function toolResult(tool: string, raw: unknown, args?: Record<string, unknown>): HTMLElement {
  const r = unwrapToolResult(raw);
  if (r == null) return el("div", { class: "tool-body" }, "—");
  if (typeof r === "object") {
    const o = r as Record<string, unknown>;
    if (tool === "sql" && Array.isArray(o.columns)) return sqlResult(o);
    if (tool === "knowledge_graph" && (o.tables || o.characteristic_terms)) return kgResult(o);
    if (tool === "schema" && Array.isArray(o.tables)) return schemaResult(o);
  }
  return el("div", { class: "tool-body" },
    typeof r === "string" ? r : JSON.stringify(r, null, 2));
}

/* sql → a real table: mono, right-aligned numbers, truncation stated */
function sqlResult(o: Record<string, unknown>): HTMLElement {
  const cols = o.columns as string[];
  const rows = (o.rows as unknown[][]) ?? [];
  const box = el("div", { class: "tr-box" });
  if (!rows.length) {
    box.append(el("div", { class: "tr-note" }, "no rows"));
    return box;
  }
  const numeric = cols.map((_, i) => rows.every((r) => typeof r[i] === "number" || r[i] == null));
  const table = el("table", { class: "tr-table" },
    el("thead", null, el("tr", null,
      cols.map((c, i) => el("th", { class: numeric[i] ? "num" : null }, c)))),
    el("tbody", null, rows.map((r) => el("tr", null,
      r.map((v, i) => {
        const s = v == null ? "∅" : String(v);
        return el("td", {
          class: numeric[i] ? "num" : null,
          title: s.length > 80 ? s : null,
        }, s.length > 80 ? s.slice(0, 80) + "…" : s);
      })))));
  box.append(el("div", { class: "tr-scroll" }, table));
  if (o.truncated) box.append(el("div", { class: "tr-note" }, "truncated — showing the first rows"));
  return box;
}

/* knowledge_graph → the corpus at a glance: tables, the term field, joins */
function kgResult(o: Record<string, unknown>): HTMLElement {
  const box = el("div", { class: "tr-box" });
  const tables = (o.tables as Record<string, unknown>[]) ?? [];
  if (tables.length) {
    box.append(el("div", { class: "tr-sub" }, "tables"),
      el("div", { class: "tr-chips" }, tables.map((t) =>
        el("span", { class: "chip tr-tbl" },
          String(t.name),
          t.rows != null || t.approx_rows != null
            ? el("i", null, ` ~${Number(t.rows ?? t.approx_rows).toLocaleString()}`)
            : null))));
  }
  const terms = (o.characteristic_terms as string[]) ?? [];
  if (terms.length) {
    // centrality order arrives most-central first: size the first ranks up
    box.append(el("div", { class: "tr-sub" }, "characteristic terms"),
      el("div", { class: "tr-terms" }, terms.slice(0, 24).map((t, i) =>
        el("span", { class: "tr-term tr-t" + (i < 4 ? 0 : i < 10 ? 1 : 2) }, t))));
  }
  const joins = (o.joins as Record<string, unknown>[]) ?? [];
  if (joins.length) {
    box.append(el("div", { class: "tr-sub" }, "join paths"),
      el("div", null, joins.map((j) => el("div", { class: "tr-join" },
        el("b", null, String(j.from).replace(/^table:/, "")),
        el("span", { class: "tr-arrow" }, ` —${j.label ?? ""}→ `),
        el("b", null, String(j.to).replace(/^table:/, "")),
        j.confidence != null
          ? el("span", { class: "tr-conf" },
            ` ${j.method === "inferred" || j.method === "inclusion" ? "inferred · " : ""}${Number(j.confidence).toFixed(2)}`)
          : null))));
  }
  if (!tables.length && !terms.length && !joins.length) {
    box.append(el("div", { class: "tr-note" }, "empty graph"));
  }
  return box;
}

/* schema → one line per table: name, rows, columns, fks */
function schemaResult(o: Record<string, unknown>): HTMLElement {
  const box = el("div", { class: "tr-box" });
  for (const t of o.tables as Record<string, unknown>[]) {
    box.append(el("div", { class: "tr-schema" },
      el("b", null, String(t.name)),
      el("span", { class: "tr-conf" }, ` ~${Number(t.approx_rows ?? 0).toLocaleString()} · `),
      el("span", { class: "tr-cols" }, (t.columns as string[] ?? []).join(", ")),
      ...((t.foreign_keys as string[] ?? []).map((fk) =>
        el("div", { class: "tr-join tr-fk" }, `↳ ${fk}`)))));
  }
  return box;
}

function renderChatRail(): void {
  const rail = document.getElementById("chatrail") as HTMLElement;
  rail.replaceChildren();

  // the rail's left edge is draggable — the pill is the affordance
  const widthGrip = el("div", { class: "rail-resize", title: "drag to resize" }, el("i"));
  widthGrip.addEventListener("pointerdown", (down: PointerEvent) => {
    down.preventDefault();
    widthGrip.setPointerCapture(down.pointerId);
    const grid = document.getElementById("bodygrid") as HTMLElement;
    const move = (e: PointerEvent) => {
      const w = Math.round(Math.min(
        720, Math.max(300, document.documentElement.clientWidth - e.clientX)));
      grid.style.setProperty("--railw", `${w}px`);
    };
    widthGrip.addEventListener("pointermove", move);
    widthGrip.addEventListener("pointerup", () => {
      widthGrip.removeEventListener("pointermove", move);
      localStorage.setItem("stemma.railw",
        grid.style.getPropertyValue("--railw") || "380px");
    }, { once: true });
  });
  rail.append(widthGrip);

  const db = state.db as string;
  const conv = activeConv(db);
  const key = `${db}:${conv}`;

  const convPick = el("select", {
    class: "input rail-convpick",
    onchange: () => {
      setActiveConv(db, convPick.value);
      renderChatRail();
    },
  });
  const newBtn = el("button", {
    class: "btn accent",
    title: "start a new chat",
    onclick: () => {
      const id = "c" + Date.now().toString(36);
      setActiveConv(db, id);
      chatLog.set(`${db}:${id}`, []);
      renderChatRail();
    },
  }, "+ new chat");
  rail.append(el("div", { class: "rail-head" },
    el("span", { class: "subhead", style: "margin:0" }, "chat"),
    el("span", { class: "sql-caption" },
      state.cfg?.lm ? `${db} · ${state.cfg.lm.model}` : "no model configured"),
    el("span", { class: "spacer" }),
    newBtn));
  rail.append(el("div", { class: "rail-convrow" }, convPick));

  // the resume list: every conversation in the store, newest first
  getJSON<{ conversations: { id: string; title: string; turns: number }[] }>(
    `/api/db/${db}/chats`).then((r) => {
      const seen = new Set<string>();
      convPick.replaceChildren();
      for (const c of r.conversations) {
        seen.add(c.id);
        convPick.append(el("option", { value: c.id, selected: c.id === conv ? "" : null },
          `${c.title || c.id} · ${Math.ceil(c.turns / 2)} turns`));
      }
      if (!seen.has(conv)) {
        convPick.append(el("option", { value: conv, selected: "" }, "(new chat)"));
      }
    }).catch(() => { /* resume list is a nicety */ });

  if (!state.cfg?.lm) {
    rail.append(el("div", { class: "rail-transcript" },
      el("div", { class: "empty" },
        "— talk to the data by proxy needs a model: set console.lm in " +
        "config.json (endpoint, model, api_key) or restart the console with " +
        "--lm-endpoint http://host:port/v1 --lm-model <name> " +
        "(any openai-compatible server: vllm, llama.cpp, litellm)")));
    return;
  }

  if (!chatLog.has(key)) {
    chatLog.set(key, []);
    getJSON<{ messages: ChatMsg[] }>(
      `/api/db/${db}/chat?conversation=${encodeURIComponent(conv)}`).then((r) => {
        const cur = chatLog.get(key) as ChatMsg[];
        if (cur.length === 0 && r.messages.length) {
          cur.push(...r.messages);
          if (chatRailOpen()) renderChatRail();
        }
      }).catch(() => { /* absent history is fine */ });
  }
  const log = chatLog.get(key) as ChatMsg[];

  const transcript = el("div", { class: "rail-transcript" });
  const input = el("textarea", {
    class: "input rail-chatinput",
    rows: "1",
    placeholder: `ask ${db} anything…`,
    onkeydown: (e: Event) => {
      const k = e as KeyboardEvent;
      if (k.key === "Enter" && !k.shiftKey) {
        k.preventDefault();
        send();
      }
    },
  });
  const savedH = localStorage.getItem("stemma.chatinputh");
  if (savedH) input.style.height = savedH;
  const sendBtn = el("button", { class: "btn accent", onclick: () => send() }, "send");

  // the input row's top edge is draggable — taller box for longer questions
  const heightGrip = el("div", { class: "rail-inputgrip", title: "drag to resize" }, el("i"));
  heightGrip.addEventListener("pointerdown", (down: PointerEvent) => {
    down.preventDefault();
    heightGrip.setPointerCapture(down.pointerId);
    const bottom = input.getBoundingClientRect().bottom;
    const move = (e: PointerEvent) => {
      const h = Math.round(Math.min(
        window.innerHeight * 0.4, Math.max(34, bottom - e.clientY)));
      input.style.height = `${h}px`;
    };
    heightGrip.addEventListener("pointermove", move);
    heightGrip.addEventListener("pointerup", () => {
      heightGrip.removeEventListener("pointermove", move);
      localStorage.setItem("stemma.chatinputh", input.style.height);
    }, { once: true });
  });

  rail.append(transcript, heightGrip,
    el("div", { class: "rail-inputrow" }, input, sendBtn));
  redraw();

  function redraw(): void {
    transcript.replaceChildren();
    if (!log.length) {
      transcript.append(el("div", { class: "empty" },
        "— every mention the model uses is pinned through resolve first; " +
        "tool calls appear here, trajectories open in the main view"));
    }
    for (const m of log) {
      if (m.role === "user") {
        transcript.append(el("div", { class: "chat-msg user" },
          el("div", { class: "who" }, "you"),
          el("div", { class: "md" }, m.content)));
      } else {
        for (const t of m.trail ?? []) transcript.append(renderTrailItem(t));
        transcript.append(el("div", { class: "chat-msg" },
          el("div", { class: "who" }, "stemma"),
          md(m.content)));
      }
    }
    transcript.scrollTop = transcript.scrollHeight;
  }

  function renderTrailItem(t: ChatTrailItem): HTMLElement {
    // resolutions render like reasoning blocks: inline, visual, collapsible
    if (t.tool === "resolve" && t.trace) {
      const trace = t.trace;
      const d = el("details", { class: "chat-tool", open: "" });
      d.append(el("summary", null,
        el("span", { class: "chip" }, "resolve"),
        `\u201c${trace.query}\u201d · ${trace.mentions.length} mention${trace.mentions.length === 1 ? "" : "s"}`));
      d.append(renderMiniTrace(trace));
      d.append(el("div", { style: "margin:3px 0 2px" },
        el("button", {
          class: "rail-showtraj",
          onclick: () => showTraceInMain(trace),
        }, "full trajectory →")));
      return d;
    }
    const d = el("details", { class: "chat-tool" });
    const label = t.tool === "sql"
      ? `sql · ${((t.args as { query?: string }).query ?? "").slice(0, 60)}`
      : t.tool;
    d.append(el("summary", null, el("span", { class: "chip" }, t.tool), label));
    d.append(toolResult(t.tool, t.result, t.args as Record<string, unknown>));
    return d;
  }

  async function send(): Promise<void> {
    const text = input.value.trim();
    if (!text) return;
    input.value = "";
    log.push({ role: "user", content: text });
    redraw();
    const wait = el("div", { class: "chat-wait" }, el("i"), el("i"), el("i"));
    transcript.append(wait);
    sendBtn.setAttribute("disabled", "");
    try {
      const r = await fetch(`/api/db/${db}/chat`, {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({
          conversation: conv,
          messages: log.map((m) => ({ role: m.role, content: m.content })),
        }),
      });
      const d = (await r.json()) as ChatResponse & { detail?: string };
      if (!r.ok) throw new Error(d.detail ?? r.statusText);
      log.push({ role: "assistant", content: d.message, trail: d.trail });
      const lastResolve = [...d.trail].reverse().find((t) => t.tool === "resolve" && t.trace);
      if (lastResolve?.trace) showTraceInMain(lastResolve.trace);
    } catch (e) {
      log.push({ role: "assistant", content: "— " + (e as Error).message, trail: [] });
    } finally {
      wait.remove();
      sendBtn.removeAttribute("disabled");
      redraw();
    }
  }
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
  const meta = tables.find((t) => t.name === name);
  // keyset pagination: a stack of page-start cursors; null = first page.
  // ?after= deep-links a page (e.g. the candidate card's "open row").
  const afterParam = params.get("after");
  const cursors: (number | null)[] = [afterParam !== null ? Number(afterParam) : null];
  let lastRowid: number | null = null;
  let hasMore = false;
  let filter = params.get("q") ?? "";

  const filterInput = el("input", {
    class: "input",
    value: filter,
    placeholder: "filter — substring across text columns (trigram-served)…",
    onkeydown: (e: Event) => {
      if ((e as KeyboardEvent).key === "Enter") {
        filter = filterInput.value.trim();
        cursors.length = 1;
        load(null);
      }
    },
  });
  const body = el("div", null);
  host.append(
    el("h1", { class: "h1" }, name),
    el("p", { class: "sql-caption" },
      meta
        ? `~${meta.row_count.toLocaleString()} rows · ` +
        meta.columns.map((c) => `${c.name} ${c.type.toLowerCase()}${c.pk ? " ·pk" : ""}`).join(" · ")
        : ""),
    el("div", { class: "data-tools" }, filterInput,
      el("button", {
        class: "btn",
        onclick: () => {
          filter = filterInput.value.trim();
          cursors.length = 1;
          load(null);
        },
      }, "filter")),
    body,
  );
  await load(cursors[0]);

  async function load(after: number | null): Promise<void> {
    body.replaceChildren(el("div", { class: "empty" }, "loading…"));
    const qs = new URLSearchParams({ limit: String(limit) });
    if (after !== null) qs.set("after", String(after));
    if (filter) qs.set("q", filter);
    const d = await getJSON<RowsPage>(
      `/api/db/${state.db}/rows/${encodeURIComponent(name!)}?${qs}`);
    hasMore = d.has_more;
    const ridIdx = d.columns.indexOf("_rowid");
    lastRowid = d.rows.length ? Number(d.rows[d.rows.length - 1][ridIdx]) : null;
    const tbl = el("table", { class: "grid" },
      el("thead", null, el("tr", null, d.columns.map((c) => el("th", null, c)))),
      el("tbody", null, d.rows.map((r) =>
        el("tr", null, r.map((v) =>
          el("td", { class: typeof v === "number" ? "num" : null }, v === null ? "∅" : v))))));
    const pager = el("div", { class: "pager" },
      el("button", {
        class: "btn",
        disabled: cursors.length <= 1 ? "" : null,
        onclick: () => {
          cursors.pop();
          load(cursors[cursors.length - 1]);
        },
      }, "‹ prev"),
      el("button", {
        class: "btn",
        disabled: hasMore ? null : "",
        onclick: () => {
          cursors.push(lastRowid);
          load(lastRowid);
        },
      }, "next ›"),
      el("span", { class: "where" },
        d.rows.length
          ? `page ${cursors.length} · ${d.rows.length} rows${filter ? ` · filtered “${filter}”` : ""}`
          : "— nothing matches"));
    body.replaceChildren(el("div", { class: "table-scroll" }, tbl), pager);
  }
}

/* ---------- graph view ---------- */

const KIND_TOGGLES = ["column", "value", "term"] as const;

async function viewGraph(host: HTMLElement): Promise<void> {
  setCrumbs("graph");
  const g = await getJSON<KnowledgeGraph>(`/api/db/${state.db}/graph`);
  host.append(
    el("h1", { class: "h1" }, "knowledge graph"),
    el("p", { class: "lede" },
      g.layer === "compiled"
        ? "two readings of the compiled graph: the map (typographic, scannable) and the diagram (spatial, force-laid). join paths — including transitive routes through intermediate tables — are computed below; click one to light the route."
        : "schema layer only — run stemma-server against this database once to compile the full graph."));
  if (!g.nodes.length) {
    host.append(el("div", { class: "empty" }, "— nothing compiled"));
    return;
  }

  const byKey = new Map(g.nodes.map((n) => [n.key, n]));
  const touching = (key: string) => g.edges.filter((e) => e.source === key || e.target === key);
  const cent = (n: GraphNode) => Number((n.props as { centrality?: number }).centrality ?? 0);
  const maxCent = Math.max(1e-6, ...g.nodes.map(cent));
  const tables = g.nodes.filter((n) => n.kind === "table");

  /* ---- transitive join paths: simple paths (≤3 hops) over declared and
   * inferred joins — the routes a query planner could take ---- */
  type JoinStep = { edge: GraphEdge; from: string; to: string };
  const joinEdges = g.edges.filter((e) => e.kind === "fk" || e.kind === "inferred_fk");
  const joinPaths: JoinStep[][] = [];
  {
    const seen = new Set<string>();
    const walk = (at: string, path: JoinStep[], visited: Set<string>) => {
      if (path.length > 0) {
        const sig = path.map((s) => `${s.from}>${s.to}:${s.edge.label}`).join("|");
        const rsig = [...path].reverse().map((s) => `${s.to}>${s.from}:${s.edge.label}`).join("|");
        if (!seen.has(sig) && !seen.has(rsig)) {
          seen.add(sig);
          joinPaths.push([...path]);
        }
      }
      if (path.length >= 3) return;
      for (const e of joinEdges) {
        const nxt = e.source === at ? e.target : e.target === at ? e.source : null;
        if (!nxt || visited.has(nxt)) continue;
        visited.add(nxt);
        path.push({ edge: e, from: at, to: nxt });
        walk(nxt, path, visited);
        path.pop();
        visited.delete(nxt);
      }
    };
    for (const t of tables) walk(t.key, [], new Set([t.key]));
  }
  joinPaths.sort((a, b) => a.length - b.length);

  /* ---- controls ---- */
  const mode = { v: localStorage.getItem("stemma.graphmode") ?? "map" };
  const modeSeg = el("span", { class: "seg" },
    ["map", "diagram"].map((m) =>
      el("button", {
        class: mode.v === m ? "on" : "",
        onclick: () => {
          mode.v = m;
          localStorage.setItem("stemma.graphmode", m);
          modeSeg.querySelectorAll("button").forEach((b, i) =>
            b.classList.toggle("on", ["map", "diagram"][i] === m));
          render();
        },
      }, m)));
  const shown = new Set<string>(["column", "value", "term"]);
  const legend = el("div", { class: "graph-legend" }, modeSeg);
  for (const k of ["column", "value", "term"]) {
    const count = g.nodes.filter((x) => x.kind === k).length;
    if (!count) continue;
    const chip = el("button", {
      class: "chip",
      onclick: () => {
        if (shown.has(k)) shown.delete(k);
        else shown.add(k);
        chip.classList.toggle("off");
        render();
      },
    }, `${k}s · ${count}`);
    legend.append(chip);
  }
  const searchBox = el("input", {
    class: "input kg-search",
    placeholder: "find in graph…",
    oninput: () => {
      const q = searchBox.value.trim().toLowerCase();
      for (const [k, elm] of labelEls) {
        const n = byKey.get(k);
        const hit = q !== "" && (n?.label ?? "").toLowerCase().includes(q);
        elm.classList.toggle("kg-hit", hit);
        elm.classList.toggle("kg-dim", q !== "" && !hit);
      }
    },
  });
  const zoomSeg = el("span", { class: "seg", style: "margin-left:auto" },
    el("button", { onclick: () => zoomBy(1 / 1.25) }, "−"),
    el("button", { onclick: () => fit() }, "fit"),
    el("button", { onclick: () => zoomBy(1.25) }, "+"));
  legend.append(searchBox, zoomSeg);

  /* ---- join-path strip ---- */
  const pathStrip = el("div", { class: "joinpaths" },
    el("span", { class: "subhead", style: "margin:0 10px 0 0" },
      `join paths · ${joinPaths.length}`));
  if (!joinPaths.length) {
    pathStrip.append(el("span", { class: "empty", style: "padding:0" },
      "— no joins declared or discovered between tables"));
  }
  let activePath: JoinStep[] | null = null;
  for (const path of joinPaths.slice(0, 14)) {
    const chainText = [
      byKey.get(path[0].from)?.label ?? "",
      ...path.map((st) => byKey.get(st.to)?.label ?? ""),
    ].join(" → ");
    const inferred = path.some((st) => st.edge.kind === "inferred_fk");
    const chipEl = el("button", {
      class: "chip joinpath" + (inferred ? " inferred" : ""),
      onclick: (e: Event) => {
        e.stopPropagation();
        activePath = activePath === path ? null : path;
        pathStrip.querySelectorAll(".joinpath").forEach((x) => x.classList.remove("on-chan"));
        if (activePath) chipEl.classList.add("on-chan");
        highlightPath();
      },
    }, chainText + (path.length > 1 ? ` · ${path.length} hops` : ""));
    hov(chipEl, path.map((st) =>
      `<b>${esc(byKey.get(st.from)?.label ?? "")} → ${esc(byKey.get(st.to)?.label ?? "")}</b> ` +
      `${esc(st.edge.label)} · ${esc(String((st.edge.props as { method?: string }).method ?? ""))}` +
      (st.edge.kind === "inferred_fk"
        ? ` · confidence ${(st.edge.props as { confidence?: number }).confidence ?? "?"}`
        : "")).join("<br>"));
    pathStrip.append(chipEl);
  }

  const detail = el("div", { class: "graph-detail", hidden: "" });
  const canvas = el("div", { class: "kg-canvas" });
  const viewport = el("div", { class: "kg-viewport" }, canvas);
  host.append(legend, pathStrip, detail, viewport);

  /* ---- pan / zoom (shared by both modes) ---- */
  let scale = 1, tx = 0, ty = 0;
  const DIAG_W = 1400, DIAG_H = 1000;
  function applyTransform(): void {
    canvas.style.transform = `translate(${tx}px, ${ty}px) scale(${scale})`;
    canvas.classList.toggle("kg-zoomed-out", scale < 0.75);
  }
  function zoomBy(f: number, cxv?: number, cyv?: number): void {
    const rect = viewport.getBoundingClientRect();
    const px = cxv ?? rect.width / 2, py = cyv ?? rect.height / 2;
    const ns = Math.min(3, Math.max(0.3, scale * f));
    tx = px - ((px - tx) / scale) * ns;
    ty = py - ((py - ty) / scale) * ns;
    scale = ns;
    applyTransform();
  }
  function fit(): void {
    const cw = mode.v === "diagram" ? DIAG_W : canvas.scrollWidth;
    const ch = mode.v === "diagram" ? DIAG_H : canvas.scrollHeight;
    const w = viewport.clientWidth - 20, h = viewport.clientHeight - 20;
    scale = Math.min(1.2, Math.min(w / Math.max(1, cw), h / Math.max(1, ch)));
    tx = Math.max(0, (viewport.clientWidth - cw * scale) / 2);
    ty = 0;
    applyTransform();
  }
  viewport.addEventListener("wheel", (e) => {
    e.preventDefault();
    hideHover();
    const rect = viewport.getBoundingClientRect();
    zoomBy(e.deltaY < 0 ? 1.12 : 1 / 1.12, e.clientX - rect.left, e.clientY - rect.top);
  }, { passive: false });
  let drag: { x: number; y: number; tx: number; ty: number } | null = null;
  viewport.addEventListener("pointerdown", (e) => {
    if ((e.target as Element).closest(".gnode, .kg-label, .kg-tablebox, button")) return;
    drag = { x: e.clientX, y: e.clientY, tx, ty };
    viewport.classList.add("dragging");
    viewport.setPointerCapture(e.pointerId);
  });
  viewport.addEventListener("pointermove", (e) => {
    if (!drag) return;
    hideHover();
    tx = drag.tx + (e.clientX - drag.x);
    ty = drag.ty + (e.clientY - drag.y);
    applyTransform();
  });
  viewport.addEventListener("pointerup", () => {
    drag = null;
    viewport.classList.remove("dragging");
  });
  viewport.addEventListener("click", () => select(null));

  /* ---- shared state ---- */
  let selectedKey: string | null = null;
  const labelEls = new Map<string, Element>();
  let edgeEls: { el: SVGElement; e: GraphEdge }[] = [];
  let mapWires: SVGElement | null = null;

  function nodeRadius(n: GraphNode): number {
    if (n.kind === "table") return 30;
    if (n.kind === "column") return 7;
    const c = Math.sqrt(cent(n) / maxCent);
    return n.key.startsWith("phrase:") ? 6 + c * 6 : 4 + c * 9;
  }
  function nodeColor(n: GraphNode): string {
    if (n.kind === "table") return "var(--ink)";
    if (n.kind === "column") return "var(--flat)";
    if (n.kind === "value") return "var(--caution)";
    if (n.key.startsWith("phrase:")) return "var(--good)";
    const mix = Math.min(85, Math.round(Math.sqrt(cent(n) / maxCent) * 85));
    return `color-mix(in srgb, var(--flat) ${100 - mix}%, var(--accent) ${mix}%)`;
  }

  function render(): void {
    hideHover();
    labelEls.clear();
    edgeEls = [];
    mapWires = null;
    if (mode.v === "diagram") renderDiagram();
    else renderMap();
    if (selectedKey && byKey.has(selectedKey)) select(byKey.get(selectedKey)!, true);
    highlightPath();
    requestAnimationFrame(fit);
  }

  /* ================= the map: typographic, scannable ================= */
  function renderMap(): void {
    const wires = svgEl("svg", { class: "kg-wires", "aria-hidden": "true" });
    mapWires = wires;
    const map = el("div", { class: "kg-map" });
    canvas.replaceChildren(wires, map);
    for (const t of tables) {
      const cell = el("div", { class: "kg-cell" });
      const header = el("div", {
        class: "kg-tablebox kg-label",
        onclick: (e: Event) => {
          e.stopPropagation();
          select(t);
        },
      },
        el("span", { class: "kg-tablename" }, t.label),
        el("span", { class: "kg-tablerows" },
          `~${Number((t.props as { rows?: number }).rows ?? 0).toLocaleString()} rows`));
      labelEls.set(t.key, header);
      cell.append(header);

      const columns = g.nodes.filter((n) => n.kind === "column" && n.key.startsWith(`column:${t.label}.`));
      const terms = g.nodes
        .filter((n) => n.kind === "term" && n.key.startsWith(`term:${t.label}:`))
        .sort((a, b) => cent(b) - cent(a));
      const phrases = g.nodes
        .filter((n) => n.kind === "term" && n.key.startsWith(`phrase:${t.label}:`))
        .sort((a, b) => cent(b) - cent(a));
      const values = g.nodes.filter((n) => n.kind === "value" && n.key.startsWith(`value:${t.label}.`));

      const section = (title: string, cls: string, ns: GraphNode[],
        style?: (n: GraphNode) => string) => {
        if (!ns.length) return;
        cell.append(el("div", { class: "subhead kg-subhead" }, title));
        const flow = el("div", { class: "kg-flow" });
        for (const n of ns) {
          const lab = el("span", {
            class: `kg-label ${cls}`,
            style: style?.(n) ?? null,
            onclick: (e: Event) => {
              e.stopPropagation();
              select(n);
            },
            onmouseenter: () => {
              for (const e2 of touching(n.key)) {
                const other = e2.source === n.key ? e2.target : e2.source;
                labelEls.get(other)?.classList.add("hood");
              }
            },
            onmouseleave: () => {
              if (selectedKey === n.key) return;
              labelEls.forEach((x, k2) => {
                if (!selectedKey || !touching(selectedKey).some((e3) =>
                  e3.source === k2 || e3.target === k2)) x.classList.remove("hood");
              });
            },
          }, n.label);
          hov(lab, `<b>${esc(n.label)}</b> · ${esc(n.kind)}<br>` +
            Object.entries(n.props).map(([k, v]) => `${esc(k)} ${esc(v)}`).join(" · "));
          labelEls.set(n.key, lab);
          flow.append(lab);
        }
        cell.append(flow);
      };
      if (shown.has("column")) section("columns", "kg-col", columns);
      if (shown.has("value")) section("frequent values", "kg-value", values);
      if (shown.has("term")) {
        section("characteristic terms · pagerank", "kg-term", terms, (n) => {
          const size = 10.5 + Math.min(5, Math.sqrt(cent(n)) * 26);
          const mix = Math.min(78, Math.round(Math.sqrt(cent(n) / maxCent) * 78));
          return `font-size: calc(${size.toFixed(1)}px * var(--fs)); ` +
            `color: color-mix(in srgb, var(--ink-soft) ${100 - mix}%, var(--accent) ${mix}%)`;
        });
        section("named entities", "kg-phrase", phrases);
      }
      map.append(cell);
    }
    requestAnimationFrame(drawMapWires);
  }

  function mapAnchor(elm: Element): { x: number; y: number; top: number } {
    const r = elm.getBoundingClientRect();
    const c = canvas.getBoundingClientRect();
    return {
      x: (r.left + r.width / 2 - c.left) / scale,
      y: (r.bottom - c.top) / scale,
      top: (r.top - c.top) / scale,
    };
  }

  function drawMapWires(): void {
    if (!mapWires) return;
    mapWires.replaceChildren();
    mapWires.setAttribute("viewBox", `0 0 ${canvas.scrollWidth} ${canvas.scrollHeight}`);
    mapWires.setAttribute("width", String(canvas.scrollWidth));
    mapWires.setAttribute("height", String(canvas.scrollHeight));
    for (const e of joinEdges) {
      const a = labelEls.get(e.source), b = labelEls.get(e.target);
      if (!a || !b) continue;
      const pa = mapAnchor(a), pb = mapAnchor(b);
      const path = svgEl("path", {
        class: `gedge kind-${e.kind}`,
        d: `M ${pa.x} ${pa.y} C ${pa.x} ${pa.y + 46}, ${pb.x} ${pb.top - 46}, ${pb.x} ${pb.top}`,
      });
      hov(path, `<b>${esc(e.kind)}</b> ${esc(e.label)}`);
      edgeEls.push({ el: path, e });
      mapWires.append(path);
    }
    // selection wires
    if (selectedKey) {
      const sel = labelEls.get(selectedKey);
      if (sel) {
        const ps = mapAnchor(sel);
        for (const e of touching(selectedKey)) {
          const otherKey = e.source === selectedKey ? e.target : e.source;
          const other = labelEls.get(otherKey);
          if (!other) continue;
          const po = mapAnchor(other);
          mapWires.append(svgEl("path", {
            class: "gedge hot",
            d: `M ${ps.x} ${ps.y} C ${ps.x} ${ps.y + 34}, ${po.x} ${po.top - 34}, ${po.x} ${po.top}`,
          }));
        }
      }
    }
    highlightPath();
  }

  /* ================= the diagram: spatial, force-laid ================= */
  function renderDiagram(): void {
    const nodes = g.nodes.filter((n) => n.kind === "table" || shown.has(n.kind));
    const keys = new Set(nodes.map((n) => n.key));
    const edges = g.edges.filter((e) => keys.has(e.source) && keys.has(e.target));
    const idx = new Map(nodes.map((n, i) => [n.key, i]));
    const pos = nodes.map(() => ({ x: 0, y: 0, vx: 0, vy: 0 }));
    const pinned = new Set<number>();
    tables.forEach((t, ti) => {
      const i = idx.get(t.key);
      if (i === undefined) return;
      const a = (2 * Math.PI * ti) / tables.length - Math.PI / 2;
      const R = tables.length > 1 ? Math.min(DIAG_W, DIAG_H) * 0.26 : 0;
      pos[i].x = DIAG_W / 2 + R * Math.cos(a);
      pos[i].y = DIAG_H / 2 + R * Math.sin(a);
      pinned.add(i);
    });
    const GOLDEN = 2.399963;
    const childCount = new Map<number, number>();
    nodes.forEach((n, i) => {
      if (pinned.has(i)) return;
      const owner = edges.find((e) => e.target === n.key && byKey.get(e.source)?.kind === "table")
        ?? edges.find((e) => e.source === n.key && byKey.get(e.target)?.kind === "table");
      const ownerKey = owner
        ? (byKey.get(owner.source)?.kind === "table" ? owner.source : owner.target)
        : tables[0]?.key;
      const oi = ownerKey !== undefined ? (idx.get(ownerKey) ?? 0) : 0;
      const k = (childCount.get(oi) ?? 0) + 1;
      childCount.set(oi, k);
      const r = 60 + 14 * Math.sqrt(k);
      pos[i].x = pos[oi].x + r * Math.cos(k * GOLDEN);
      pos[i].y = pos[oi].y + r * Math.sin(k * GOLDEN);
    });
    const collide = nodes.map((n) => Math.max(nodeRadius(n) + 6, n.label.length * 2.6 + 6));
    const rest = (e: GraphEdge) =>
      e.kind === "fk" || e.kind === "inferred_fk" ? 420
        : e.kind === "has_column" ? 110
          : e.kind === "cooccurs" ? 150 : 170;
    for (let it = 0; it < 220; it++) {
      for (let i = 0; i < nodes.length; i++) {
        for (let j = i + 1; j < nodes.length; j++) {
          let dx = pos[j].x - pos[i].x, dy = pos[j].y - pos[i].y;
          let d2 = dx * dx + dy * dy;
          if (d2 < 1) { dx = ((i * 7 + j) % 13) - 6; dy = ((i * 5 + j) % 11) - 5; d2 = dx * dx + dy * dy; }
          const d = Math.sqrt(d2);
          const minD = collide[i] + collide[j];
          let f = 900 / d2;
          if (d < minD) f += (minD - d) * 0.06;
          const fx = (dx / d) * f, fy = (dy / d) * f;
          if (!pinned.has(i)) { pos[i].vx -= fx; pos[i].vy -= fy; }
          if (!pinned.has(j)) { pos[j].vx += fx; pos[j].vy += fy; }
        }
      }
      for (const e of edges) {
        const a = idx.get(e.source)!, b = idx.get(e.target)!;
        const dx = pos[b].x - pos[a].x, dy = pos[b].y - pos[a].y;
        const d = Math.max(1, Math.hypot(dx, dy));
        const f = (d - rest(e)) * 0.015;
        const fx = (dx / d) * f, fy = (dy / d) * f;
        if (!pinned.has(a)) { pos[a].vx += fx; pos[a].vy += fy; }
        if (!pinned.has(b)) { pos[b].vx -= fx; pos[b].vy -= fy; }
      }
      for (let i = 0; i < nodes.length; i++) {
        if (pinned.has(i)) continue;
        pos[i].x += Math.max(-18, Math.min(18, pos[i].vx));
        pos[i].y += Math.max(-18, Math.min(18, pos[i].vy));
        pos[i].vx *= 0.6;
        pos[i].vy *= 0.6;
        pos[i].x = Math.max(30, Math.min(DIAG_W - 30, pos[i].x));
        pos[i].y = Math.max(30, Math.min(DIAG_H - 30, pos[i].y));
      }
    }

    const svg = svgEl("svg", {
      class: "graph-svg", viewBox: `0 0 ${DIAG_W} ${DIAG_H}`,
      width: DIAG_W, height: DIAG_H, role: "img", "aria-label": "knowledge graph",
    });
    svg.append(svgEl("defs", null,
      svgEl("marker", {
        id: "arrow", viewBox: "0 0 8 8", refX: 7, refY: 4,
        markerWidth: 6, markerHeight: 6, orient: "auto",
      }, svgEl("path", { d: "M 0 0 L 8 4 L 0 8 z", fill: "var(--flat)" }))));
    for (const e of edges) {
      const a = pos[idx.get(e.source)!], b = pos[idx.get(e.target)!];
      const mx = (a.x + b.x) / 2 + (a.y - b.y) * 0.06;
      const my = (a.y + b.y) / 2 + (b.x - a.x) * 0.06;
      const path = svgEl("path", {
        class: `gedge kind-${e.kind}`,
        d: `M ${a.x} ${a.y} Q ${mx} ${my} ${b.x} ${b.y}`,
        ...(e.kind === "fk" || e.kind === "inferred_fk" ? { "marker-end": "url(#arrow)" } : {}),
      });
      if (e.label) {
        hov(path, `<b>${esc(e.kind)}</b> ${esc(e.label)}`);
      }
      edgeEls.push({ el: path, e });
      svg.append(path);
      if (e.kind === "fk" || e.kind === "inferred_fk") {
        svg.append(svgEl("text", { class: "gedge-label", x: mx, y: my, "text-anchor": "middle" }, e.label));
      }
    }
    nodes.forEach((n, i) => {
      const p = pos[i];
      const r = nodeRadius(n);
      const grp = svgEl("g", {
        class: `gnode kind-${n.kind}` + (n.key.startsWith("phrase:") ? " is-phrase" : ""),
        transform: `translate(${p.x}, ${p.y})`,
        cursor: "pointer",
      });
      grp.append(svgEl("circle", { r, fill: nodeColor(n), class: "gdot" }));
      if (n.kind === "table") {
        grp.append(
          svgEl("text", { class: "glabel glabel-table", y: 4, "text-anchor": "middle" }, n.label),
          svgEl("text", { class: "grows", y: r + 14, "text-anchor": "middle" },
            `~${Number((n.props as { rows?: number }).rows ?? 0).toLocaleString()} rows`));
      } else {
        grp.append(svgEl("text", {
          class: "glabel" + (r < 7 ? " glabel-small" : ""), y: r + 11, "text-anchor": "middle",
        }, n.label));
      }
      grp.addEventListener("click", (ev) => {
        ev.stopPropagation();
        select(n);
      });
      grp.addEventListener("mouseenter", () => {
        for (const { el: pe, e } of edgeEls) {
          if (e.source === n.key || e.target === n.key) pe.classList.add("hot");
        }
        for (const e of touching(n.key)) {
          const other = e.source === n.key ? e.target : e.source;
          labelEls.get(other)?.classList.add("hood");
        }
      });
      grp.addEventListener("mouseleave", () => {
        if (selectedKey === n.key) return;
        edgeEls.forEach(({ el: pe, e }) => {
          if (selectedKey && (e.source === selectedKey || e.target === selectedKey)) return;
          pe.classList.remove("hot");
        });
        labelEls.forEach((x, k2) => {
          if (!selectedKey || !touching(selectedKey).some((e3) =>
            e3.source === k2 || e3.target === k2)) x.classList.remove("hood");
        });
      });
      hov(grp, `<b>${esc(n.label)}</b> · ${esc(n.kind)}<br>` +
        Object.entries(n.props).map(([k, v]) => `${esc(k)} ${esc(v)}`).join(" · "));
      labelEls.set(n.key, grp);
      svg.append(grp);
    });
    canvas.replaceChildren(svg);
  }

  /* ---- path + selection highlighting, mode-agnostic ---- */
  function highlightPath(): void {
    edgeEls.forEach(({ el: x }) => x.classList.remove("path-hot"));
    labelEls.forEach((x) => x.classList.remove("path-hood"));
    if (!activePath) return;
    const involvedTables = new Set<string>([activePath[0].from]);
    for (const st of activePath) involvedTables.add(st.to);
    for (const k of involvedTables) labelEls.get(k)?.classList.add("path-hood");
    for (const st of activePath) {
      for (const { el: pe, e } of edgeEls) {
        if (e === st.edge) pe.classList.add("path-hot");
      }
    }
  }

  function select(n: GraphNode | null, keep = false): void {
    hideHover();
    labelEls.forEach((x) => x.classList.remove("sel", "hood"));
    edgeEls.forEach(({ el: x }) => x.classList.remove("hot"));
    if (!n) {
      if (!keep) selectedKey = null;
      detail.hidden = true;
      if (mode.v === "map") drawMapWires();
      return;
    }
    selectedKey = n.key;
    labelEls.get(n.key)?.classList.add("sel");
    const around = touching(n.key);
    for (const { el: pe, e } of edgeEls) {
      if (e.source === n.key || e.target === n.key) pe.classList.add("hot");
    }
    for (const e of around) {
      const other = e.source === n.key ? e.target : e.source;
      labelEls.get(other)?.classList.add("hood");
    }
    detail.hidden = false;
    detail.replaceChildren(
      el("span", { class: "kindtag" }, n.kind),
      el("span", { class: "name" }, n.label),
      el("span", { class: "props" },
        Object.entries(n.props).map(([k, v]) => `${k} ${v}`).join(" · ") || "—"),
      el("span", { class: "props" }, `${around.length} edge${around.length === 1 ? "" : "s"}`),
    );
    if (n.kind === "table") {
      detail.append(el("button", {
        class: "btn accent",
        onclick: () => {
          location.hash = "#/data/" + encodeURIComponent(n.label);
        },
      }, "browse data →"));
    } else if (n.kind === "term" || n.kind === "value") {
      detail.append(el("button", {
        class: "btn accent",
        onclick: () => {
          location.hash = "#/query?d=nl&q=" + encodeURIComponent(n.label);
        },
      }, `resolve \u201c${n.label}\u201d →`));
    }
    if (mode.v === "map") drawMapWires();
  }

  render();
}

/* ---------- router ---------- */

async function route(): Promise<void> {
  const hash = location.hash || "#/query";
  const [path, qs] = hash.slice(2).split("?");
  const [view, arg] = path.split("/");
  const params = new URLSearchParams(qs ?? "");
  hideHover();
  closeAllMenus();
  // legacy routes fold into the query view; #/chat opens the rail instead
  if (view === "chat") {
    setChatRail(true);
    location.hash = "#/query";
    return;
  }
  const mapped = view === "resolve" ? "query" : view === "sql" ? "query" : view || "query";
  if (view === "sql") params.set("d", "sql");
  state.view = mapped;

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
    else viewQuery(host, params);
  } catch (e) {
    host.append(el("div", { class: "sql-error" }, (e as Error).message));
  }
}

/* ---------- boot ---------- */

(async function boot() {
  initPickers();
  const cfg = await getJSON<Config>("/api/config");
  state.cfg = cfg;
  state.dbs = cfg.databases;
  state.db = cfg.databases[0] ?? null;
  (document.getElementById("topsearch") as HTMLInputElement).addEventListener("keydown", (ev) => {
    const e = ev as KeyboardEvent;
    const target = ev.target as HTMLInputElement;
    if (e.key === "Enter" && target.value.trim()) {
      location.hash = "#/query?d=nl&q=" + encodeURIComponent(target.value);
    }
  });
  (document.getElementById("chattoggle") as HTMLButtonElement).addEventListener(
    "click", () => setChatRail(Boolean((document.getElementById("chatrail") as HTMLElement).hidden)));
  if (chatRailOpen()) setChatRail(true);
  // clicking the current view's nav entry re-renders it (a stale or errored
  // view heals instead of doing nothing)
  document.querySelectorAll<HTMLAnchorElement>("#nav a").forEach((a) =>
    a.addEventListener("click", () => {
      if (a.getAttribute("href") === location.hash) route();
    }));
  globalThis.addEventListener("hashchange", route);
  pollHealth();
  route();
})();
