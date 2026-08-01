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

/* per-database chat transcripts, session-local */
type ChatMsg = { role: "user" | "assistant"; content: string; trail?: ChatTrailItem[] };
const chatLog = new Map<string, ChatMsg[]>();

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
  const mentionSpans = trace.mentions.map((i) => trace.spans[i]);

  const qline = el("div", { class: "qline" });
  const covered = (pos: number) => mentionSpans.find((s) => pos >= s.start && pos < s.end);
  let cursor = 0;
  const tokenNodes = new Map<number, HTMLElement>();
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
            `${c.table}.${c.column} #${c.rowid} (${c.score.toFixed(2)})`).join(" · ")
          : "—"));
    if (s.candidates.length) {
      hov(row, s.candidates.map((c) =>
        `<b>${esc(c.table)}.${esc(c.column)}</b> #${c.rowid} “${esc(c.snippet || c.value)}”<br>` +
        `score ${c.score.toFixed(3)} · ${esc(c.reject_reason)}`).join("<hr>"));
    }
    alsoBox.append(row);
  }
  if (!also.length) {
    alsoBox.append(el("div", { class: "empty" }, "— every considered span became a mention"));
  }

  out.append(
    el("div", { class: "sql-caption" },
      `resolved in ${trace.elapsed_ms.toFixed(1)} ms · ${trace.spans.length} spans enumerated · channels: exact, bm25, trigram, kg`),
    traj,
    alsoBox,
  );

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

function renderCandidate(c: TraceCandidate, rank: number): HTMLElement {
  const mid = c.is_doc && c.snippet
    ? snippetNode(c.snippet)
    : el("span", { class: "cand-val", title: c.value },
      `“${c.value}${c.value_truncated ? "…" : ""}”`);
  const row = el("div", { class: "cand " + (c.selected ? (rank === 0 ? "sel-0" : "sel") : "rej") },
    el("span", { class: "cand-id" },
      `${c.table}.${c.column} `, el("span", { class: "rowid" }, `#${c.rowid}`)),
    mid,
    el("span", { class: "cand-right" },
      el("span", { class: "cand-chips" },
        c.channels.map((ch) => el("span", { class: "chip" },
          ch.channel === "kg" ? `kg +${ch.raw}` : ch.channel)),
        c.selected ? null : el("span", { class: "pill bad" },
          (c.reject_reason || "rejected").replace(/_/g, " "))),
      el("span", { style: "display:flex; gap:6px; align-items:center" },
        el("span", { class: "meter" },
          el("span", { style: `width:${Math.round(c.score * 100)}%` })),
        el("span", { class: "score" }, c.score.toFixed(2)))));
  hov(row, c.channels.map((ch) =>
    ch.channel === "kg"
      ? `<b>kg</b> co-occurring terms matched: ${ch.raw}`
      : `<b>${esc(ch.channel)}</b> rank ${ch.rank + 1} · raw ${ch.raw.toFixed(3)}`).join("<br>") +
    (c.selected ? "" : `<hr>${esc(c.reject_reason.replace(/_/g, " "))}`));
  return row;
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
  rail.hidden = !open;
  grid.classList.toggle("chat-open", open);
  btn.classList.toggle("accent", open);
  hideHover();
  if (open) renderChatRail();
}

function renderChatRail(): void {
  const rail = document.getElementById("chatrail") as HTMLElement;
  rail.replaceChildren();
  rail.append(el("div", { class: "rail-head" },
    el("span", { class: "subhead", style: "margin:0" }, "chat"),
    el("span", { class: "sql-caption" },
      state.cfg?.lm ? `${state.db} · ${state.cfg.lm.model}` : "no model configured")));

  if (!state.cfg?.lm) {
    rail.append(el("div", { class: "rail-transcript" },
      el("div", { class: "empty" },
        "— talk to the data by proxy needs a model: restart the console with " +
        "--lm-endpoint http://host:port/v1 --lm-model <name> " +
        "(any openai-compatible server: vllm, llama.cpp, litellm; " +
        "bearer token via LM_API_KEY)")));
    return;
  }

  const db = state.db as string;
  if (!chatLog.has(db)) chatLog.set(db, []);
  const log = chatLog.get(db) as ChatMsg[];

  const transcript = el("div", { class: "rail-transcript" });
  const input = el("input", {
    class: "input",
    placeholder: `ask ${db} anything…`,
    onkeydown: (e: Event) => {
      if ((e as KeyboardEvent).key === "Enter") send();
    },
  });
  const sendBtn = el("button", { class: "btn accent", onclick: () => send() }, "send");
  rail.append(transcript, el("div", { class: "rail-inputrow" }, input, sendBtn));
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
          el("div", { class: "md" }, m.content)));
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
        `“${trace.query}” · ${trace.mentions.length} mention${trace.mentions.length === 1 ? "" : "s"}`));
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
    d.append(el("div", { class: "tool-body" }, JSON.stringify(t.result, null, 2)));
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
          messages: log.map((m) => ({ role: m.role, content: m.content })),
        }),
      });
      const d = (await r.json()) as ChatResponse & { detail?: string };
      if (!r.ok) throw new Error(d.detail ?? r.statusText);
      log.push({ role: "assistant", content: d.message, trail: d.trail });
      // chat drives the visual: the newest resolution opens in the main view
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
  // keyset pagination: a stack of page-start cursors; null = first page
  const cursors: (number | null)[] = [null];
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
  await load(null);

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
        ? "a map, not a scatter: each table anchors its columns, frequent values, characteristic terms (sized by pagerank centrality) and named entities. joins draw between tables — solid declared, dashed amber inferred. select anything to light its neighborhood; drag to pan, wheel to zoom."
        : "schema layer only — run stemma-server against this database once to compile the full graph."));

  if (!g.nodes.length) {
    host.append(el("div", { class: "empty" }, "— nothing compiled"));
    return;
  }

  const byKey = new Map(g.nodes.map((n) => [n.key, n]));
  const touching = (key: string) => g.edges.filter((e) => e.source === key || e.target === key);
  const cent = (n: GraphNode) => Number((n.props as { centrality?: number }).centrality ?? 0);

  const shown = new Set<string>(["column", "value", "term"]);
  const legend = el("div", { class: "graph-legend" });
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
  const zoomSeg = el("span", { class: "seg", style: "margin-left:auto" },
    el("button", { onclick: () => zoomBy(1 / 1.25) }, "−"),
    el("button", { onclick: () => fit() }, "fit"),
    el("button", { onclick: () => zoomBy(1.25) }, "+"));
  legend.append(zoomSeg);

  const detail = el("div", { class: "graph-detail", hidden: "" });
  const wires = svgEl("svg", { class: "kg-wires", "aria-hidden": "true" });
  const map = el("div", { class: "kg-map" });
  const canvas = el("div", { class: "kg-canvas" }, wires, map);
  const viewport = el("div", { class: "kg-viewport" }, canvas);
  host.append(legend, detail, viewport);

  let scale = 1, tx = 0, ty = 0;
  let selectedKey: string | null = null;
  const labelEls = new Map<string, HTMLElement>();

  function applyTransform(): void {
    canvas.style.transform = `translate(${tx}px, ${ty}px) scale(${scale})`;
  }

  function zoomBy(f: number, cxv?: number, cyv?: number): void {
    const rect = viewport.getBoundingClientRect();
    const px = cxv ?? rect.width / 2, py = cyv ?? rect.height / 2;
    const ns = Math.min(3, Math.max(0.35, scale * f));
    // keep the point under the cursor stationary
    tx = px - ((px - tx) / scale) * ns;
    ty = py - ((py - ty) / scale) * ns;
    scale = ns;
    applyTransform();
  }

  function fit(): void {
    scale = Math.min(1, (viewport.clientWidth - 24) / Math.max(1, canvas.scrollWidth));
    tx = 0;
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
    if ((e.target as HTMLElement).closest(".kg-label, .kg-tablebox, button")) return;
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

  function render(): void {
    hideHover();
    labelEls.clear();
    map.replaceChildren();
    const tables = g.nodes.filter((n) => n.kind === "table");
    for (const t of tables) {
      const cell = el("div", { class: "kg-cell" });
      const header = el("div", {
        class: "kg-tablebox kg-label",
        "data-key": t.key,
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

      const kids = touching(t.key)
        .filter((e) => e.source === t.key)
        .map((e) => byKey.get(e.target))
        .filter((n): n is GraphNode => !!n);
      const columns = kids.filter((n) => n.kind === "column");
      const terms = g.nodes
        .filter((n) => n.kind === "term" && n.key.startsWith(`term:${t.label}:`))
        .sort((a, b) => cent(b) - cent(a));
      const phrases = g.nodes
        .filter((n) => n.kind === "term" && n.key.startsWith(`phrase:${t.label}:`))
        .sort((a, b) => cent(b) - cent(a));
      const values = g.nodes.filter(
        (n) => n.kind === "value" && n.key.startsWith(`value:${t.label}.`));

      const section = (title: string, cls: string, nodes: GraphNode[],
        style?: (n: GraphNode) => string) => {
        if (!nodes.length) return;
        cell.append(el("div", { class: "subhead kg-subhead" }, title));
        const flow = el("div", { class: "kg-flow" });
        for (const n of nodes) {
          const lab = el("span", {
            class: `kg-label ${cls}`,
            "data-key": n.key,
            style: style?.(n) ?? null,
            onclick: (e: Event) => {
              e.stopPropagation();
              select(n);
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
          return `font-size: calc(${size.toFixed(1)}px * var(--fs))`;
        });
        section("named entities", "kg-phrase", phrases);
      }
      map.append(cell);
    }
    requestAnimationFrame(drawWires);
    if (selectedKey && byKey.has(selectedKey)) select(byKey.get(selectedKey)!, true);
  }

  /* content-space anchor of a label, independent of pan/zoom */
  function anchor(elm: HTMLElement): { x: number; y: number; top: number } {
    const r = elm.getBoundingClientRect();
    const c = canvas.getBoundingClientRect();
    return {
      x: (r.left + r.width / 2 - c.left) / scale,
      y: (r.bottom - c.top) / scale,
      top: (r.top - c.top) / scale,
    };
  }

  function drawWires(): void {
    wires.replaceChildren();
    wires.setAttribute("viewBox", `0 0 ${canvas.scrollWidth} ${canvas.scrollHeight}`);
    wires.setAttribute("width", String(canvas.scrollWidth));
    wires.setAttribute("height", String(canvas.scrollHeight));
    // joins between tables, always visible
    for (const e of g.edges.filter((x) => x.kind === "fk" || x.kind === "inferred_fk")) {
      const a = labelEls.get(e.source), b = labelEls.get(e.target);
      if (!a || !b) continue;
      const pa = anchor(a), pb = anchor(b);
      const path = svgEl("path", {
        class: `gedge kind-${e.kind}`,
        d: `M ${pa.x} ${pa.y} C ${pa.x} ${pa.y + 46}, ${pb.x} ${pb.top - 46}, ${pb.x} ${pb.top}`,
      });
      hov(path, `<b>${esc(e.kind)}</b> ${esc(e.label)}`);
      wires.append(path);
    }
    // the selected node's neighborhood
    if (selectedKey) {
      const sel = labelEls.get(selectedKey);
      if (sel) {
        const ps = anchor(sel);
        for (const e of touching(selectedKey)) {
          const otherKey = e.source === selectedKey ? e.target : e.source;
          const other = labelEls.get(otherKey);
          if (!other) continue;
          const po = anchor(other);
          wires.append(svgEl("path", {
            class: "gedge hot",
            d: `M ${ps.x} ${ps.y} C ${ps.x} ${ps.y + 34}, ${po.x} ${po.top - 34}, ${po.x} ${po.top}`,
          }));
        }
      }
    }
  }

  viewport.addEventListener("click", () => select(null));

  function select(n: GraphNode | null, keep = false): void {
    hideHover();
    for (const elm of labelEls.values()) elm.classList.remove("sel", "hood");
    if (!n) {
      if (!keep) selectedKey = null;
      detail.hidden = true;
      drawWires();
      return;
    }
    selectedKey = n.key;
    labelEls.get(n.key)?.classList.add("sel");
    const around = touching(n.key);
    for (const e of around) {
      const otherKey = e.source === n.key ? e.target : e.source;
      labelEls.get(otherKey)?.classList.add("hood");
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
      }, `resolve "${n.label}" →`));
    }
    drawWires();
  }

  render();
  requestAnimationFrame(fit);
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
  globalThis.addEventListener("hashchange", route);
  pollHealth();
  route();
})();
