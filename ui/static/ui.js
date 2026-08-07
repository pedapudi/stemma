// src/ui.ts
function el(tag, attrs, ...children) {
  const n = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs ?? {})) {
    if (v === null || v === void 0) continue;
    if (k === "class") n.className = String(v);
    else if (k.startsWith("on") && typeof v === "function") {
      n.addEventListener(k.slice(2), v);
    } else n.setAttribute(k, String(v));
  }
  appendChildren(n, children);
  return n;
}
function svgEl(tag, attrs, ...children) {
  const n = document.createElementNS("http://www.w3.org/2000/svg", tag);
  for (const [k, v] of Object.entries(attrs ?? {})) n.setAttribute(k, String(v));
  appendChildren(n, children);
  return n;
}
function appendChildren(n, children) {
  for (const c of children) {
    if (c === null || c === void 0) continue;
    if (Array.isArray(c)) appendChildren(n, c);
    else n.append(c instanceof Node ? c : document.createTextNode(String(c)));
  }
}
async function getJSON(url) {
  const r = await fetch(url);
  if (!r.ok) {
    let detail = r.statusText;
    try {
      detail = (await r.json()).detail ?? detail;
    } catch (_e) {
    }
    throw new Error(detail);
  }
  return r.json();
}
function esc(s) {
  return String(s).replace(/[&<>"']/g, (c) => ({
    "&": "&amp;",
    "<": "&lt;",
    ">": "&gt;",
    '"': "&quot;",
    "'": "&#39;"
  })[c]);
}
function md(text) {
  const root = el("div", {
    class: "md-root"
  });
  const blocks = text.split(/```/);
  blocks.forEach((block, bi) => {
    if (bi % 2 === 1) {
      root.append(el("pre", {
        class: "md-code"
      }, block.replace(/^\w*\n/, "")));
      return;
    }
    let list = null;
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
        root.append(el("div", {
          class: `md-h md-h${h[1].length}`
        }, ...mdInline(h[2])));
      } else if (li) {
        if (!list) {
          list = el("ul", {
            class: "md-list"
          });
          root.append(list);
        }
        list.append(el("li", null, ...mdInline(li[1])));
      } else {
        list = null;
        root.append(el("p", {
          class: "md-p"
        }, ...mdInline(line)));
      }
    }
  });
  return root;
}
function mdInline(text) {
  const out = [];
  const re = /(`[^`]+`)|(\*\*[^*]+\*\*)|(\*[^*]+\*)|(\[[^\]]+\]\(https?:[^)]+\))/g;
  let last = 0;
  for (let m = re.exec(text); m; m = re.exec(text)) {
    if (m.index > last) out.push(text.slice(last, m.index));
    const t = m[0];
    if (m[1]) out.push(el("code", {
      class: "md-codespan"
    }, t.slice(1, -1)));
    else if (m[2]) out.push(el("b", null, t.slice(2, -2)));
    else if (m[3]) out.push(el("i", null, t.slice(1, -1)));
    else if (m[4]) {
      const mm = t.match(/^\[([^\]]+)\]\((https?:[^)]+)\)$/);
      if (mm) out.push(el("a", {
        href: mm[2],
        target: "_blank",
        rel: "noreferrer"
      }, mm[1]));
    }
    last = m.index + t.length;
  }
  if (last < text.length) out.push(text.slice(last));
  return out;
}
function hovCandidate(c) {
  const snip = esc((c.snippet || c.value).slice(0, 170)).replace(/⟨/g, '<b class="hc-hit">').replace(/⟩/g, "</b>");
  const chips = c.channels.map((ch) => `<span class="hc-ch hc-ch-${esc(ch.channel)}">${esc(ch.channel)} \xB7 ${ch.raw.toFixed(1)}</span>`).join("");
  return `
    <div class="hc-head">
      <span class="hc-ref">${esc(c.table)}.${esc(c.column)}</span>
      <span class="hc-rowid">#${c.rowid}</span>
      <span class="hc-score">${c.score.toFixed(2)}</span>
    </div>
    <div class="hc-meter"><i style="width:${Math.round(c.score * 100)}%"></i></div>
    <div class="hc-snip">${snip}</div>
    <div class="hc-chips">${chips}</div>` + (c.coherence ? `<div class="hc-coh">\u2B21 ${esc(c.coherence)}</div>` : "") + (c.adjudicated ? '<div class="hc-adj">\u2696 adjudicated \u2014 the lm chose this among near-ties</div>' : "") + (c.selected ? '<div class="hc-verdict hc-ok">selected</div>' : `<div class="hc-verdict hc-no">${esc((c.reject_reason || "rejected").replace(/_/g, " "))}</div>`) + '<div class="hc-hint">click for the card</div>';
}
function snippetNode(snippet) {
  const out = el("span", {
    class: "snippet"
  });
  const parts = snippet.split(/⟨([^⟩]*)⟩/);
  parts.forEach((p, i) => {
    if (i % 2 === 1) out.append(el("span", {
      class: "hit"
    }, p));
    else if (p) out.append(p);
  });
  return out;
}
var hovercard = document.getElementById("hovercard");
function hideHover() {
  hovercard.classList.remove("on");
}
function hov(node, html) {
  node.addEventListener("mouseenter", () => {
    hovercard.innerHTML = html;
    hovercard.classList.add("on");
  });
  node.addEventListener("mousemove", (ev) => {
    const e = ev;
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
var state = {
  cfg: null,
  dbs: [],
  db: null,
  schema: null,
  view: "query"
};
var chatLog = /* @__PURE__ */ new Map();
function activeConv(db) {
  return localStorage.getItem(`stemma.conv.${db}`) ?? "default";
}
function setActiveConv(db, conv) {
  localStorage.setItem(`stemma.conv.${db}`, conv);
}
var COLOR_THEMES = [
  [
    "monokai",
    "monokai",
    [
      "#1e1f1c",
      "#272822",
      "#f8f8f2",
      "#a6e22e",
      "#f92672",
      "#66d9ef"
    ]
  ],
  [
    "solarized-dark",
    "solarized dark",
    [
      "#04222B",
      "#0A2D38",
      "#93A1A1",
      "#8BB80E",
      "#E0483C",
      "#2AA198"
    ]
  ],
  [
    "solarized-light",
    "solarized light",
    [
      "#FDF6E3",
      "#FBF1D6",
      "#586E75",
      "#6B9B0B",
      "#DC322F",
      "#268BD2"
    ]
  ],
  [
    "google-light",
    "google light",
    [
      "#FFFFFF",
      "#F4F4F4",
      "#474A4E",
      "#34A853",
      "#EA4335",
      "#1B9CB8"
    ]
  ],
  [
    "google-dark",
    "google dark",
    [
      "#202124",
      "#2C2D30",
      "#FFFFFF",
      "#34A853",
      "#EA4335",
      "#24C1E0"
    ]
  ],
  [
    "lunaria-light",
    "lunaria light",
    [
      "#EBE4E1",
      "#E2DCD9",
      "#363434",
      "#497D46",
      "#783C1F",
      "#3778A9"
    ]
  ],
  [
    "lunaria-eclipse",
    "lunaria eclipse",
    [
      "#323F46",
      "#3B484F",
      "#DFE2ED",
      "#BEDBC1",
      "#BA9088",
      "#C8429F"
    ]
  ],
  [
    "belafonte-day",
    "belafonte day",
    [
      "#D5CCBA",
      "#CCC3B2",
      "#34292D",
      "#6E6A4E",
      "#BE100E",
      "#426A79"
    ]
  ],
  [
    "belafonte-night",
    "belafonte night",
    [
      "#20111B",
      "#271821",
      "#D5CCBA",
      "#A6A07A",
      "#D6403E",
      "#6F8E97"
    ]
  ],
  [
    "paper",
    "paper",
    [
      "#F2EEDE",
      "#E6E2D3",
      "#1A1A1A",
      "#216609",
      "#CC3E28",
      "#1E6FCC"
    ]
  ],
  [
    "zenburn",
    "zenburn",
    [
      "#3A3A3A",
      "#424241",
      "#DCDCCC",
      "#8FB28F",
      "#CC9393",
      "#8CD0D3"
    ]
  ],
  [
    "selenized-black",
    "selenized black",
    [
      "#181818",
      "#202020",
      "#DEDEDE",
      "#83C746",
      "#FF5E56",
      "#56D8C9"
    ]
  ],
  [
    "relaxed",
    "relaxed",
    [
      "#353A44",
      "#3D424B",
      "#F7F7F7",
      "#A0AC77",
      "#BC5653",
      "#7EAAC7"
    ]
  ],
  [
    "espresso",
    "espresso",
    [
      "#323232",
      "#3A3A3A",
      "#FFFFFF",
      "#A5C261",
      "#D25252",
      "#6C99BB"
    ]
  ],
  [
    "dracula",
    "dracula",
    [
      "#282A36",
      "#343746",
      "#F8F8F2",
      "#50FA7B",
      "#FF5555",
      "#BD93F9"
    ]
  ],
  [
    "ubuntu",
    "ubuntu",
    [
      "#300A24",
      "#3D1530",
      "#EEEEEC",
      "#8AE234",
      "#CC0000",
      "#34E2E2"
    ]
  ]
];
var MONO_GSM = '"Google Sans Mono","Noto Sans Mono",ui-monospace,monospace';
var SANS_GROTESK = '"Space Grotesk",system-ui,sans-serif';
var TYPE_OPTIONS = [
  {
    id: "T7",
    label: "Google Sans Mono",
    group: "technical",
    head: MONO_GSM,
    body: MONO_GSM
  },
  {
    id: "T9",
    label: "Source Sans 3 + Source Code Pro",
    group: "technical",
    head: '"Source Sans 3",system-ui,sans-serif',
    body: '"Source Sans 3",system-ui,sans-serif'
  },
  {
    id: "T12",
    label: "Inconsolata",
    group: "technical",
    head: '"Inconsolata",ui-monospace,monospace',
    body: '"Inconsolata",ui-monospace,monospace'
  },
  {
    id: "T14",
    label: "Ubuntu + Ubuntu Mono",
    group: "technical",
    head: '"Ubuntu",system-ui,sans-serif',
    body: '"Ubuntu",system-ui,sans-serif'
  },
  {
    id: "E5",
    label: "Fraunces",
    group: "editorial",
    head: '"Fraunces",Georgia,serif',
    body: '"Fraunces",Georgia,serif'
  },
  {
    id: "E7",
    label: "Bitter",
    group: "editorial",
    head: '"Bitter",Georgia,serif',
    body: '"Bitter",Georgia,serif'
  },
  {
    id: "E8",
    label: "Literata",
    group: "editorial",
    head: '"Literata",Georgia,serif',
    body: '"Literata",Georgia,serif'
  },
  {
    id: "E15",
    label: "Domine",
    group: "editorial",
    head: '"Domine",Georgia,serif',
    body: '"Domine",Georgia,serif'
  },
  {
    id: "D2",
    label: "Archivo Narrow + Space Grotesk",
    group: "display",
    head: `"Archivo Narrow",${SANS_GROTESK}`,
    body: SANS_GROTESK
  },
  {
    id: "D12",
    label: "Hanken Grotesk",
    group: "display",
    head: '"Hanken Grotesk",system-ui,sans-serif',
    body: '"Hanken Grotesk",system-ui,sans-serif'
  },
  {
    id: "D14",
    label: "Barlow Condensed + Space Grotesk",
    group: "display",
    head: '"Barlow Condensed","Archivo Narrow",system-ui,sans-serif',
    body: SANS_GROTESK
  },
  {
    id: "D5",
    label: "Bricolage Grotesque",
    group: "display",
    head: '"Bricolage Grotesque",system-ui,sans-serif',
    body: '"Bricolage Grotesque",system-ui,sans-serif'
  }
];
var SAMPLE_LINE = "the quick brown fox 0123";
var FONT_SIZES = [
  [
    "s",
    1
  ],
  [
    "m",
    1.15
  ],
  [
    "l",
    1.3
  ]
];
var openMenus = [];
function closeAllMenus(except) {
  for (const m of openMenus) {
    if (m !== except) m.classList.remove("open");
  }
}
function swatchStrip(colors) {
  const strip = el("span", {
    class: "swatch-strip",
    "aria-hidden": "true"
  });
  for (const c of colors) {
    const chip = el("i");
    chip.style.background = c;
    strip.append(chip);
  }
  return strip;
}
function buildThemePicker() {
  const mount = document.getElementById("themepicker");
  const saved = localStorage.getItem("stemma.theme") ?? "paper";
  document.documentElement.dataset.theme = saved;
  const current = COLOR_THEMES.find((t) => t[0] === saved) ?? COLOR_THEMES[9];
  const trigName = el("span", {
    class: "cd-name"
  }, current[1]);
  const trigStrip = el("span", null, swatchStrip(current[2]));
  const list = el("div", {
    class: "cd-list",
    role: "listbox"
  });
  const cd = el("span", {
    class: "cd"
  }, el("button", {
    class: "cd-trigger",
    onclick: (e) => {
      e.stopPropagation();
      hideHover();
      closeAllMenus(cd);
      cd.classList.toggle("open");
    }
  }, trigStrip, trigName, el("span", {
    class: "cd-caret"
  }, "\u25BE")), list);
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
      }
    }, swatchStrip(colors), el("span", {
      class: "cd-name"
    }, label));
    list.append(opt);
  }
  openMenus.push(cd);
  mount.append(cd);
}
function buildTypePicker() {
  const mount = document.getElementById("typepicker");
  const saved = localStorage.getItem("stemma.type") ?? "T9";
  if (saved !== "T9") document.documentElement.dataset.type = saved;
  const savedSize = localStorage.getItem("stemma.fontsize") ?? "s";
  const sizeVal = FONT_SIZES.find(([k]) => k === savedSize)?.[1] ?? 1;
  if (sizeVal !== 1) document.documentElement.style.setProperty("--fs", String(sizeVal));
  const current = TYPE_OPTIONS.find((o) => o.id === saved) ?? TYPE_OPTIONS[1];
  const trigName = el("span", {
    class: "cd-name"
  }, current.label);
  const trigSpec = el("span", {
    class: "tf-spec"
  }, "Ag");
  trigSpec.style.fontFamily = current.head;
  const listBox = el("div", {
    class: "tf-list"
  });
  const pop = el("div", {
    class: "tf-pop"
  }, listBox);
  const cd = el("span", {
    class: "cd"
  }, el("button", {
    class: "cd-trigger",
    onclick: (e) => {
      e.stopPropagation();
      hideHover();
      closeAllMenus(cd);
      cd.classList.toggle("open");
    }
  }, trigSpec, trigName, el("span", {
    class: "cd-caret"
  }, "\u25BE")), pop);
  for (const group of [
    "technical",
    "editorial",
    "display"
  ]) {
    listBox.append(el("div", {
      class: "tf-group"
    }, el("div", {
      class: "subhead"
    }, group)));
    for (const o of TYPE_OPTIONS.filter((x) => x.group === group)) {
      const name = el("span", {
        class: "tf-name"
      }, o.label);
      name.style.fontFamily = o.head;
      const sample = el("span", {
        class: "tf-sample",
        "aria-hidden": "true"
      }, SAMPLE_LINE);
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
        }
      }, name, sample);
      listBox.append(opt);
    }
  }
  const seg = el("span", {
    class: "seg"
  });
  for (const [k, v] of FONT_SIZES) {
    const b = el("button", {
      class: k === savedSize ? "on" : "",
      onclick: () => {
        document.documentElement.style.setProperty("--fs", String(v));
        localStorage.setItem("stemma.fontsize", k);
        seg.querySelectorAll("button").forEach((x) => x.classList.remove("on"));
        b.classList.add("on");
      }
    }, k);
    seg.append(b);
  }
  pop.append(el("div", {
    class: "tf-foot"
  }, el("span", {
    class: "k"
  }, "text size"), seg));
  openMenus.push(cd);
  mount.append(cd);
}
function initPickers() {
  buildThemePicker();
  buildTypePicker();
  document.addEventListener("click", () => closeAllMenus());
}
async function pollHealth() {
  const s = document.getElementById("status");
  const w = document.getElementById("statusword");
  try {
    const h = await getJSON("/api/health");
    s.className = "status " + (h.grpc ? "ok" : "down");
    w.textContent = h.grpc ? "live" : "grpc down";
  } catch (_e) {
    s.className = "status down";
    w.textContent = "ui down";
  }
  setTimeout(pollHealth, 8e3);
}
function renderSidebar() {
  const side = document.getElementById("sidebar");
  side.replaceChildren();
  if (state.dbs.length > 1) {
    side.append(el("div", {
      class: "tree-group"
    }, "database"));
    for (const d of state.dbs) {
      side.append(el("button", {
        class: "tree-node" + (d === state.db ? " sel" : ""),
        onclick: () => {
          state.db = d;
          state.schema = null;
          route();
          if (chatRailOpen()) renderChatRail();
        }
      }, el("span", {
        class: "tree-icon"
      }), d));
    }
  }
  side.append(el("div", {
    class: "tree-group"
  }, "tables"));
  if (!state.schema) {
    side.append(el("div", {
      class: "empty"
    }, "loading\u2026"));
    return;
  }
  const current = location.hash.startsWith("#/data/") ? decodeURIComponent(location.hash.slice(7).split("?")[0]) : null;
  for (const t of state.schema.tables) {
    side.append(el("button", {
      class: "tree-node" + (t.name === current ? " sel" : ""),
      onclick: () => {
        location.hash = "#/data/" + encodeURIComponent(t.name);
      }
    }, el("span", {
      class: "tree-icon round"
    }), t.name, el("span", {
      class: "count"
    }, "~" + t.row_count.toLocaleString())));
  }
  const storeBox = el("div", {
    class: "side-store"
  }, el("div", {
    class: "tree-group"
  }, "store"));
  side.append(storeBox);
  getJSON(`/api/db/${state.db}/store`).then((m) => {
    if (!m.exists) {
      storeBox.append(el("div", {
        class: "empty"
      }, "\u2014 not created yet"));
      return;
    }
    const pairs = [
      [
        "size",
        ((m.size_bytes ?? 0) / 1e6).toFixed(1) + " mb"
      ],
      [
        "lexical",
        m.lexical ? m.lexical.values.toLocaleString() + " values" : "\u2014"
      ],
      [
        "kg",
        m.kg ? `${m.kg.nodes} nodes \xB7 ${m.kg.edges} edges` : "\u2014"
      ],
      [
        "embed queue",
        String(m.embed_queue ?? 0)
      ],
      [
        "vectors",
        (m.model_registry ?? []).length ? `${(m.model_registry ?? []).length} tables` : "none \xB7 m3"
      ]
    ];
    storeBox.append(el("div", {
      class: "kv"
    }, pairs.map(([k, v]) => [
      el("span", {
        class: "k"
      }, k),
      el("span", {
        class: "v"
      }, v)
    ])));
  }).catch(() => storeBox.append(el("div", {
    class: "empty"
  }, "\u2014 unavailable")));
}
function setCrumbs(...parts) {
  document.getElementById("crumbs").textContent = [
    state.db,
    ...parts
  ].filter(Boolean).join(" \xB7 ");
}
function viewQuery(host, params) {
  const dialect = params.get("d") === "sql" ? "sql" : "nl";
  const q = params.get("q") ?? "";
  setCrumbs("query", dialect === "sql" ? "sql" : "natural");
  const seg = el("span", {
    class: "seg"
  }, el("button", {
    class: dialect === "nl" ? "on" : "",
    onclick: () => {
      location.hash = "#/query?d=nl" + (q ? "&q=" + encodeURIComponent(q) : "");
    }
  }, "natural"), el("button", {
    class: dialect === "sql" ? "on" : "",
    onclick: () => {
      location.hash = "#/query?d=sql";
    }
  }, "sql"));
  host.append(el("div", {
    style: "display:flex; align-items:baseline; gap:14px"
  }, el("h1", {
    class: "h1"
  }, "query"), seg), el("p", {
    class: "lede"
  }, dialect === "nl" ? "the dialect is natural language: mentions resolve to records, and the trajectory shows every span considered, every channel fired, and every candidate \u2014 chosen and near-miss alike." : "the dialect is sql, read-only: main is the .stemmadb store, src is the user database. every query ships with its plan."));
  if (dialect === "nl") queryNatural(host, q);
  else querySql(host);
}
function queryNatural(host, q) {
  const input = el("input", {
    class: "input",
    value: q,
    placeholder: "ask about the data \u2014 mentions resolve to records\u2026",
    onkeydown: (e) => {
      if (e.key === "Enter") run(input.value);
    }
  });
  const out = el("div", null);
  const examplesRow = el("div", null);
  host.append(el("div", {
    class: "queryrow"
  }, input, el("button", {
    class: "btn accent",
    onclick: () => run(input.value)
  }, "resolve")), examplesRow, out);
  getJSON(`/api/db/${state.db}/history`).then((r) => {
    if (!r.queries.length) return;
    const row = el("div", null, el("span", {
      class: "sql-caption",
      style: "margin-right:8px"
    }, "recent:"));
    for (const x of r.queries.slice(0, 6)) {
      row.append(el("button", {
        class: "chip",
        style: "margin-right:6px; cursor:pointer",
        onclick: () => {
          input.value = x;
          run(x);
        }
      }, x.length > 60 ? x.slice(0, 60) + "\u2026" : x));
    }
    examplesRow.after(row);
  }).catch(() => {
  });
  getJSON(`/api/db/${state.db}/examples`).then((r) => {
    for (const x of r.examples) {
      examplesRow.append(el("button", {
        class: "chip",
        style: "margin-right:6px; cursor:pointer",
        onclick: () => {
          input.value = x;
          run(x);
        }
      }, x));
    }
    if (r.examples.length) {
      examplesRow.prepend(el("span", {
        class: "sql-caption",
        style: "margin-right:8px"
      }, "from the kg:"));
    }
  }).catch(() => {
  });
  if (q) run(q);
  async function run(query) {
    if (!query.trim()) return;
    history.replaceState(null, "", "#/query?d=nl&q=" + encodeURIComponent(query));
    document.getElementById("topsearch").value = query;
    out.replaceChildren(el("div", {
      class: "empty"
    }, "resolving\u2026"));
    let trace;
    if (pendingTrace && pendingTrace.query === query) {
      trace = pendingTrace;
      pendingTrace = null;
      renderTrace(out, trace);
      return;
    }
    try {
      trace = await getJSON(`/api/db/${state.db}/resolve?q=` + encodeURIComponent(query));
    } catch (e) {
      out.replaceChildren(el("div", {
        class: "sql-error"
      }, "resolution failed \u2014 " + e.message));
      return;
    }
    renderTrace(out, trace);
  }
}
function querySql(host) {
  const box = el("textarea", {
    class: "input sqlbox mono",
    placeholder: "SELECT \u2026"
  }, "SELECT src_table, src_column, count(*) AS n\nFROM lex_values GROUP BY 1, 2 ORDER BY n DESC");
  const out = el("div", null);
  const run = async () => {
    out.replaceChildren(el("div", {
      class: "empty"
    }, "running\u2026"));
    try {
      const r = await fetch(`/api/db/${state.db}/sql`, {
        method: "POST",
        headers: {
          "content-type": "application/json"
        },
        body: JSON.stringify({
          sql: box.value
        })
      });
      const d = await r.json();
      if (!r.ok) throw new Error(d.detail ?? r.statusText);
      out.replaceChildren(el("div", {
        class: "sql-caption"
      }, `${d.rows.length} row${d.rows.length === 1 ? "" : "s"}${d.truncated ? " (truncated)" : ""} \xB7 ${d.elapsed_ms} ms`), renderPlan(d.plan), el("div", {
        class: "table-scroll"
      }, el("table", {
        class: "grid"
      }, el("thead", null, el("tr", null, d.columns.map((c) => el("th", null, c)))), el("tbody", null, d.rows.map((row) => el("tr", null, row.map((v) => el("td", null, v === null ? "\u2205" : v))))))));
    } catch (e) {
      out.replaceChildren(el("div", {
        class: "sql-error"
      }, e.message));
    }
  };
  box.addEventListener("keydown", (e) => {
    if ((e.ctrlKey || e.metaKey) && e.key === "Enter") run();
  });
  host.append(box, el("div", {
    style: "margin-top:8px"
  }, el("button", {
    class: "btn accent",
    onclick: run
  }, "run"), el("span", {
    class: "sql-caption",
    style: "margin-left:10px"
  }, "ctrl-enter runs")), out);
}
function renderPlan(plan) {
  const box = el("div", {
    class: "plan panel"
  }, el("div", {
    class: "subhead"
  }, "query plan"));
  if (!plan.length) {
    box.append(el("div", {
      class: "empty"
    }, "\u2014 trivial plan"));
    return box;
  }
  for (const p of plan) {
    const opClass = /^SCAN/.test(p.detail) ? "scan" : /^SEARCH/.test(p.detail) ? "search" : "";
    box.append(el("div", {
      class: "plan-row"
    }, el("span", {
      class: "tick"
    }, "\u2502 ".repeat(p.depth) + "\u251C\u2500"), el("span", {
      class: `op ${opClass}`
    }, p.detail)));
  }
  return box;
}
function renderTrace(out, trace) {
  out.replaceChildren();
  hideHover();
  const mentionSpans = trace.mentions.map((i) => trace.spans[i]);
  const TABLE_HUES = [
    "var(--caution)",
    "var(--brand-accent)",
    "var(--bad)",
    "var(--good)",
    "var(--flat)"
  ];
  const tablesSeen = [
    ...new Set(trace.spans.flatMap((s) => s.candidates.map((c) => c.table)))
  ].sort();
  const hueOf = (t) => TABLE_HUES[tablesSeen.indexOf(t) % TABLE_HUES.length];
  const qline = el("div", {
    class: "qline"
  });
  const covered = (pos) => mentionSpans.find((s) => pos >= s.start && pos < s.end);
  const spanTok = /* @__PURE__ */ new Map();
  let cursor = 0;
  for (const t of trace.tokens) {
    if (t.start > cursor) qline.append(trace.query.slice(cursor, t.start));
    const m = covered(t.start);
    const node = el("span", {
      class: "qtok" + (m ? " mention" : t.stopword ? " stop" : "")
    }, t.text);
    if (m && !spanTok.has(m.id)) spanTok.set(m.id, node);
    qline.append(node);
    cursor = t.end;
  }
  if (cursor < trace.query.length) qline.append(trace.query.slice(cursor));
  const subline = el("div", {
    class: "subline"
  });
  const spanChip = /* @__PURE__ */ new Map();
  cursor = 0;
  const emitPlain = (from, to) => {
    if (to > from) subline.append(el("span", {
      class: "sub-plain"
    }, trace.query.slice(from, to)));
  };
  for (const sp of mentionSpans) {
    emitPlain(cursor, sp.start);
    const top = sp.candidates.find((c) => c.selected);
    let chip;
    if (sp.ambiguous) {
      const readings = sp.candidates.filter((c) => c.selected);
      const seen = /* @__PURE__ */ new Set();
      const distinct = readings.filter((c) => {
        const k = `${c.table}.${c.column}`;
        if (seen.has(k)) return false;
        seen.add(k);
        return true;
      }).slice(0, 3);
      chip = el("span", {
        class: "sub-fork"
      }, el("span", {
        class: "sub-fork-q",
        title: "distinct readings tie \u2014 which did you mean?"
      }, "?"), el("span", {
        class: "sub-fork-set"
      }, distinct.map((c) => {
        const b = el("button", {
          class: "sub-chip sub-fork-chip",
          title: `${c.table}.${c.column} #${c.rowid}`,
          onclick: (e) => {
            e.stopPropagation();
            showCard(sp, c);
          }
        }, tablesSeen.length > 1 ? el("i", {
          class: "chip-dot",
          style: `background:${hueOf(c.table)}`
        }) : null, `${c.table}.${c.column}`, c.row_count && Number(c.row_count) > 1 ? el("i", {
          class: "fork-count"
        }, ` \xD7${c.row_count}`) : null);
        hov(b, hovCandidate(c));
        return b;
      })));
    } else if (top) {
      const label = top.is_doc ? `${top.table} #${top.rowid}` : `\u201C${top.value.length > 28 ? top.value.slice(0, 28) + "\u2026" : top.value}\u201D`;
      chip = el("button", {
        class: "sub-chip",
        title: `${top.table}.${top.column} #${top.rowid}`,
        onclick: (e) => {
          e.stopPropagation();
          showCard(sp, top);
        }
      }, tablesSeen.length > 1 ? el("i", {
        class: "chip-dot",
        style: `background:${hueOf(top.table)}`
      }) : null, label);
      hov(chip, hovCandidate(top));
    } else {
      chip = el("span", {
        class: "sub-chip sub-unresolved",
        title: "unresolved"
      }, sp.text);
    }
    spanChip.set(sp.id, chip);
    subline.append(chip);
    cursor = sp.end;
  }
  emitPlain(cursor, trace.query.length);
  const wires = svgEl("svg", {
    class: "traj-wires",
    "aria-hidden": "true"
  });
  const lineage = el("div", {
    class: "lineage"
  }, wires, qline, subline);
  function drawLineage() {
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
        d: `M ${x1} ${y1} C ${x1} ${(y1 + y2) / 2}, ${x2} ${(y1 + y2) / 2}, ${x2} ${y2}`
      }));
    }
  }
  requestAnimationFrame(drawLineage);
  const card = el("div", {
    class: "cand-card",
    hidden: ""
  });
  function showCard(sp, c) {
    hideHover();
    card.hidden = false;
    card.replaceChildren(el("div", {
      class: "cc-head"
    }, el("span", {
      class: "cand-id"
    }, `${c.table}.${c.column} `, el("span", {
      class: "rowid"
    }, `#${c.rowid}`)), el("span", {
      class: "score"
    }, c.score.toFixed(3)), c.selected ? el("span", {
      class: "pill pending"
    }, "selected") : el("span", {
      class: "pill bad"
    }, (c.reject_reason || "rejected").replace(/_/g, " ")), el("span", {
      class: "spacer"
    }), el("button", {
      class: "btn",
      onclick: () => {
        card.hidden = true;
      }
    }, "\u2715")), el("div", {
      class: "cc-body"
    }, c.is_doc && c.snippet ? snippetNode(c.snippet) : el("span", {
      class: "mono"
    }, `\u201C${c.value}\u201D`)), el("div", {
      class: "cc-channels"
    }, c.channels.map((ch) => el("span", {
      class: "chip"
    }, `${ch.channel} \xB7 rank ${ch.rank + 1} \xB7 ${ch.raw.toFixed(2)}`))), el("div", {
      class: "cc-actions"
    }, el("button", {
      class: "btn accent",
      onclick: () => {
        location.hash = `#/data/${encodeURIComponent(c.table)}?after=${Number(c.rowid) - 1}`;
      }
    }, "open row in data \u2192"), c.is_doc ? null : el("button", {
      class: "btn",
      onclick: () => {
        location.hash = "#/query?d=nl&q=" + encodeURIComponent(c.value);
      }
    }, "resolve this value \u2192"), el("span", {
      class: "sql-caption"
    }, `for mention \u201C${sp.text}\u201D`)));
  }
  const also = trace.spans.filter((s) => s.status !== "selected" && s.status !== "skipped").sort((a, b) => a.start - b.start);
  const alsoBox = el("details", {
    class: "alsoran section"
  }, el("summary", {
    class: "subhead",
    style: "cursor:pointer; display:inline-block"
  }, `spans considered \xB7 ${also.length}`));
  const statusPill = {
    overlapped: [
      "neutral",
      "overlapped"
    ],
    weak: [
      "caution",
      "weak"
    ],
    no_candidates: [
      "neutral",
      "no match"
    ]
  };
  for (const s of also) {
    const [tone, label] = statusPill[s.status] ?? [
      "neutral",
      s.status
    ];
    const row = el("div", {
      class: "alsoran-row"
    }, el("span", {
      class: "spantext"
    }, `\u201C${s.text}\u201D`), el("span", {
      class: "pill " + tone
    }, label), el("span", {
      class: "alsoran-cands"
    }, s.candidates.length ? s.candidates.map((c) => `${c.table}.${c.column} #${c.rowid} (${c.score.toFixed(2)})`).join(" \xB7 ") : "\u2014"));
    if (s.candidates.length) {
      hov(row, s.candidates.slice(0, 3).map(hovCandidate).join("<hr>"));
    }
    alsoBox.append(row);
  }
  if (!also.length) {
    alsoBox.append(el("div", {
      class: "empty"
    }, "\u2014 every considered span became a mention"));
  }
  const cohPairs = [];
  for (let i = 0; i < mentionSpans.length; i++) {
    for (let j = i + 1; j < mentionSpans.length; j++) {
      const ca = mentionSpans[i].candidates.find((c) => c.selected && c.coherence);
      const cb = mentionSpans[j].candidates.find((c) => c.selected && c.coherence);
      if (ca && cb && ca.coherence === cb.coherence) {
        cohPairs.push({
          a: mentionSpans[i].id,
          b: mentionSpans[j].id,
          label: ca.coherence
        });
      }
    }
  }
  if (cohPairs.length) lineage.classList.add("has-coh");
  function drawCoherence() {
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
        d: `M ${x1} ${y} C ${x1} ${dip}, ${x2} ${dip}, ${x2} ${y}`
      }));
      const t = svgEl("text", {
        class: "coh-label",
        x: (x1 + x2) / 2,
        y: dip + 12,
        "text-anchor": "middle"
      });
      t.textContent = `\u2B21 ${p.label}`;
      wires.append(t);
    }
  }
  requestAnimationFrame(drawCoherence);
  const panels = {
    anatomy: {
      node: el("div", {
        class: "anatomy"
      }),
      built: false,
      build: buildAnatomy
    },
    space: {
      node: el("div", {
        class: "space"
      }),
      built: false,
      build: buildSpace
    }
  };
  const MODE_KEY = "stemma.trajmode";
  let mode = localStorage.getItem(MODE_KEY) || "anatomy";
  if (!(mode in panels)) mode = "anatomy";
  const modeBtns = /* @__PURE__ */ new Map();
  const modeBar = el(
    "div",
    {
      class: "traj-modes"
    },
    el("span", {
      class: "sql-caption",
      style: "margin-right:6px"
    }, "view:"),
    ...Object.keys(panels).map((m) => {
      const b = el("button", {
        class: "chip" + (m === mode ? " on-chan" : ""),
        onclick: () => setMode(m)
      }, m);
      modeBtns.set(m, b);
      return b;
    }),
    ...tablesSeen.length > 1 ? [
      el("span", {
        class: "spacer",
        style: "max-width:18px"
      }),
      ...tablesSeen.map((t) => el("span", {
        class: "tbl-key mono"
      }, el("i", {
        style: `background:${hueOf(t)}`
      }), t))
    ] : []
  );
  function setMode(m) {
    mode = m;
    localStorage.setItem(MODE_KEY, m);
    hideHover();
    for (const [k, p] of Object.entries(panels)) {
      if (k === m && !p.built) {
        p.build();
        p.built = true;
      }
      p.node.hidden = k !== m;
      modeBtns.get(k)?.classList.toggle("on-chan", k === m);
    }
  }
  const b2c = (() => {
    const m = /* @__PURE__ */ new Map([
      [
        0,
        0
      ]
    ]);
    let b = 0;
    const enc = new TextEncoder();
    [
      ...trace.query
    ].forEach((ch, i) => {
      b += enc.encode(ch).length;
      m.set(b, i + 1);
    });
    return (byte) => m.get(byte) ?? byte;
  })();
  function buildAnatomy() {
    const host = panels.anatomy.node;
    const tokensIn = (sp) => trace.tokens.filter((t) => t.start >= sp.start && t.end <= sp.end).length;
    host.append(el("div", {
      class: "subhead"
    }, "span lattice"), el("div", {
      class: "sql-caption"
    }, "every span the pipeline enumerated \xB7 winners tile the query, the rest lost their range"));
    const lat = el("div", {
      class: "lattice mono"
    });
    lat.append(el("div", {
      class: "lat-q"
    }, trace.query));
    const byLen = /* @__PURE__ */ new Map();
    for (const sp of trace.spans) {
      const n = tokensIn(sp);
      if (!byLen.has(n)) byLen.set(n, []);
      byLen.get(n).push(sp);
    }
    const lens = [
      ...byLen.keys()
    ].sort((a, b) => b - a);
    for (const n of lens) {
      const spans = byLen.get(n).sort((a, b) => a.start - b.start);
      const laneEnds = [];
      const lanes = spans.map((sp) => {
        const i = laneEnds.findIndex((e) => e <= sp.start);
        const lane = i === -1 ? laneEnds.length : i;
        laneEnds[lane] = sp.end;
        return lane;
      });
      const track = el("div", {
        class: "lat-track",
        style: `height:${(Math.max(...lanes) + 1) * 13}px`
      });
      spans.forEach((sp, k) => {
        const top = sp.candidates[0];
        const isMention = trace.mentions.includes(sp.id);
        const cls = "lat-bar lat-" + (isMention ? "won" : sp.status);
        const bar = el("button", {
          class: cls,
          style: `left:${b2c(sp.start)}ch; width:${Math.max(1, b2c(sp.end) - b2c(sp.start))}ch; top:${lanes[k] * 13}px;` + (top && sp.status !== "skipped" ? `--w:${Math.round(Math.min(1, top.score) * 100)}%; --th:${hueOf(top.table)}` : ""),
          onclick: top ? () => showCard(sp, top) : null
        });
        if (top) bar.append(el("i", {
          class: "lat-fill"
        }));
        hov(bar, top ? `<div class="hc-head"><span class="hc-ref">\u201C${esc(sp.text)}\u201D</span><span class="hc-score">${top.score.toFixed(2)}</span></div>` + hovCandidate(top) : `<div class="hc-head"><span class="hc-ref">\u201C${esc(sp.text)}\u201D</span></div><div class="hc-verdict hc-no">${esc(sp.status.replace(/_/g, " "))}</div>`);
        track.append(bar);
      });
      lat.append(el("div", {
        class: "lat-row"
      }, el("span", {
        class: "lat-lab"
      }, n > MAX_LAT_N ? "whole" : String(n)), track));
    }
    lat.append(el("div", {
      class: "lat-legend sql-caption"
    }, el("i", {
      class: "lat-key lat-won"
    }), " mention \xB7 ", el("i", {
      class: "lat-key lat-overlapped"
    }), " overlapped \xB7 ", el("i", {
      class: "lat-key lat-weak"
    }), " weak \xB7 ", el("i", {
      class: "lat-key lat-no_candidates"
    }), " no match \xB7 ", el("i", {
      class: "lat-key lat-skipped"
    }), " skipped"));
    host.append(lat);
    host.append(el("div", {
      class: "subhead",
      style: "margin-top:18px"
    }, "verdicts"), el("div", {
      class: "sql-caption"
    }, "per mention: what the evidence was, which mechanism decided it, and who lost"));
    for (const sp of mentionSpans) host.append(verdict(sp));
    if (!mentionSpans.length) {
      host.append(el("div", {
        class: "empty"
      }, "\u2014 no mentions, no verdicts"));
    }
  }
  function stages(sp, c) {
    const spanChars = [
      ...sp.text
    ].length;
    const nonKg = c.channels.filter((ch) => ch.channel !== "kg");
    const rrf = nonKg.reduce((s, ch) => s + 1 / (4 + ch.rank), 0);
    const base = Math.min(rrf * 4 / 3, 1);
    const hasExact = c.channels.some((ch) => ch.channel === "exact");
    const valChars = Math.max(1, [
      ...c.value
    ].length + (c.value_truncated ? 40 : 0));
    const branch = hasExact ? Math.min(0.9 + 0.1 * base, 1) : c.is_doc ? Math.min(base * 0.85, 0.85) : base * (0.4 + 0.6 * Math.sqrt(spanChars / Math.max(valChars, spanChars)));
    const cos = c.channels.filter((ch) => ch.channel === "dense").reduce((m, ch) => Math.max(m, ch.raw), -1);
    const calibrated = cos >= 0 ? Math.min(Math.max((cos - 0.3) / 0.3, 0), 1) * 0.78 : 0;
    return {
      base,
      branch,
      cos,
      calibrated,
      hasExact
    };
  }
  function verdict(sp) {
    const w = sp.candidates.find((c) => c.selected) ?? sp.candidates[0];
    const box = el("div", {
      class: "why"
    });
    if (sp.ambiguous) {
      const readings = sp.candidates.filter((c) => c.selected).slice(0, 4);
      box.append(el("div", {
        class: "why-head"
      }, el("span", {
        class: "sf-mention"
      }, sp.text), el("span", {
        class: "why-arrow"
      }, "\u2192"), el("span", {
        class: "pill caution"
      }, "ambiguous")));
      box.append(el("div", {
        class: "why-line"
      }, el("span", {
        class: "why-k"
      }, "undecided"), el("span", null, "distinct readings tie \u2014 context, cards and the adjudicator could not separate them; ask which is meant")));
      for (const c of readings) {
        const row = el("div", {
          class: "why-line"
        }, el("span", {
          class: "why-k"
        }, "reading"), el("button", {
          class: "why-ref mono",
          style: `color:${hueOf(c.table)}`,
          onclick: () => showCard(sp, c)
        }, `${c.table}.${c.column} #${c.rowid}`), el("span", {
          class: "why-soft mono"
        }, ` \u201C${c.value.slice(0, 40)}\u201D` + (c.row_count && Number(c.row_count) > 1 ? ` \xB7 \xD7${c.row_count}` : "")));
        hov(row, hovCandidate(c));
        box.append(row);
      }
      return box;
    }
    if (!w) {
      box.append(el("div", {
        class: "why-head"
      }, el("span", {
        class: "sf-mention"
      }, sp.text), el("span", {
        class: "sql-caption"
      }, " \u2014 unresolved")));
      return box;
    }
    const s = stages(sp, w);
    const mechHue = s.hasExact ? "var(--good)" : s.calibrated > s.branch + 5e-3 ? "color-mix(in srgb, var(--brand-accent) 60%, var(--ink))" : "var(--flat)";
    const meter = el("span", {
      class: "why-meter"
    }, el("i", {
      style: `width:${Math.round(Math.min(1, w.score) * 100)}%`
    }));
    const head = el("div", {
      class: "why-head"
    }, el("span", {
      class: "sf-mention"
    }, sp.text), el("span", {
      class: "why-arrow"
    }, "\u2192"), el("button", {
      class: "why-ref mono",
      style: `color:${hueOf(w.table)}`,
      onclick: () => showCard(sp, w)
    }, `${w.table}.${w.column} #${w.rowid}`), meter, el("span", {
      class: "why-score mono"
    }, w.score.toFixed(2)));
    box.append(head);
    if (w.is_doc && w.snippet) {
      box.append(el("div", {
        class: "why-snip"
      }, snippetNode(w.snippet)));
    } else if (!w.is_doc) {
      box.append(el("div", {
        class: "why-snip mono"
      }, `\u201C${w.value}\u201D`));
    }
    box.append(el("div", {
      class: "why-line"
    }, el("span", {
      class: "why-k"
    }, "evidence"), el("span", {
      class: "hc-chips"
    }, w.channels.map((ch) => {
      const chip = el("span", {
        class: `hc-ch hc-ch-${ch.channel}`
      }, ch.channel === "dense" ? `dense \xB7 cos ${ch.raw.toFixed(2)}` : ch.channel === "kg" ? `kg +${ch.raw.toFixed(2)}` : `${ch.channel} \xB7 rank ${ch.rank + 1}`);
      return chip;
    }))));
    const mechDot = () => el("i", {
      class: "why-dot",
      style: `background:${mechHue}`
    });
    const mech = [];
    if (s.hasExact) {
      mech.push(el("div", {
        class: "why-line"
      }, el("span", {
        class: "why-k"
      }, "decided by"), mechDot(), el("span", null, "exact match \u2014 the mention equals the stored value, floor 0.9")));
    } else if (s.calibrated > s.branch + 5e-3) {
      mech.push(el("div", {
        class: "why-line"
      }, el("span", {
        class: "why-k"
      }, "decided by"), mechDot(), el("span", null, `semantic floor \u2014 cos ${s.cos.toFixed(2)} calibrates to ${s.calibrated.toFixed(2)}, above the lexical case (${s.branch.toFixed(2)})`)));
    } else {
      mech.push(el("div", {
        class: "why-line"
      }, el("span", {
        class: "why-k"
      }, "decided by"), mechDot(), el("span", null, w.is_doc ? "fused lexical evidence under document scoring (length is not held against it)" : "fused lexical evidence with length affinity")));
    }
    if (w.coherence) {
      mech.push(el("div", {
        class: "why-line why-coh"
      }, el("span", {
        class: "why-k"
      }, "coherence"), el("span", {
        class: "mono"
      }, `\u2B21 ${w.coherence} `), el("span", {
        class: "why-soft"
      }, "\u2014 verified in the data, +0.15")));
    }
    if (w.adjudicated) {
      mech.push(el("div", {
        class: "why-line why-adj"
      }, el("span", {
        class: "why-k"
      }, "adjudicated"), el("span", null, "\u2696 the lm chose this among near-ties (gap < 0.08)")));
    }
    box.append(...mech);
    const r = sp.candidates.filter((c) => c !== w)[0];
    if (r) {
      const rs = stages(sp, r);
      let why;
      if (w.coherence && !r.coherence) why = "no verified join path";
      else if (s.hasExact && !rs.hasExact) why = "no exact match";
      else if (s.calibrated > s.branch && rs.cos < s.cos) {
        why = rs.cos >= 0 ? `weaker semantic match (cos ${rs.cos.toFixed(2)})` : "no dense evidence";
      } else if (r.channels.length < w.channels.length) why = "fewer channels agreed";
      else why = "lower fused evidence";
      const rlabel = r.is_doc ? `${r.table} #${r.rowid}` : `\u201C${r.value.slice(0, 32)}\u201D`;
      const rrow = el("div", {
        class: "why-line why-rival"
      }, el("span", {
        class: "why-k"
      }, "beat"), el("button", {
        class: "why-ref mono",
        onclick: () => showCard(sp, r)
      }, `${rlabel} \xB7 ${r.score.toFixed(2)}`), el("span", {
        class: "why-soft"
      }, ` \u2014 ${why}`));
      hov(rrow, hovCandidate(r));
      box.append(rrow);
    } else {
      box.append(el("div", {
        class: "why-line why-rival"
      }, el("span", {
        class: "why-k"
      }, "beat"), el("span", {
        class: "why-soft"
      }, "no rival \u2014 the only candidate")));
    }
    return box;
  }
  function buildSpace() {
    const host = panels.space.node;
    const best = /* @__PURE__ */ new Map();
    for (const sp of trace.spans) {
      for (const c of sp.candidates) {
        const cos = c.channels.filter((ch) => ch.channel === "dense").reduce((m, ch) => Math.max(m, ch.raw), -1);
        if (cos < 0) continue;
        const k = `${c.table}#${c.rowid}`;
        const prev = best.get(k);
        if (!prev || cos > prev.cos || c.selected && !prev.c.selected) {
          best.set(k, {
            c,
            cos,
            sp
          });
        }
      }
    }
    const hits = [
      ...best.values()
    ].sort((a, b) => b.cos - a.cos);
    host.append(el("div", {
      class: "subhead"
    }, "semantic spectrum"), el("div", {
      class: "sql-caption"
    }, "every dense-retrieved record on the cosine axis \xB7 right is nearer the query \xB7 one lane per table"));
    if (!hits.length) {
      host.append(el("div", {
        class: "empty"
      }, "\u2014 no dense evidence in this trajectory: the embedder was absent, or every span had exact lexical anchors"));
      return;
    }
    const LO = 0.26, HI = 0.74;
    const xOf = (cos) => Math.max(1.5, Math.min(98.5, (cos - LO) / (HI - LO) * 100));
    const spec = el("div", {
      class: "spec"
    });
    const marks = [
      [
        0.3,
        "0.30 \xB7 calibration floor"
      ],
      [
        0.4,
        "0.40"
      ],
      [
        0.5,
        "0.50"
      ],
      [
        0.6,
        "0.60 \xB7 strong"
      ]
    ];
    const grid = el("div", {
      class: "spec-grid"
    });
    for (const [cos, label] of marks) {
      grid.append(el("i", {
        class: "spec-rule",
        style: `left:${xOf(cos)}%`
      }));
      grid.append(el("span", {
        class: "spec-rulelab",
        style: `left:${xOf(cos)}%`
      }, label));
    }
    spec.append(grid);
    const tables = [
      ...new Set(hits.map((h) => h.c.table))
    ].sort((a, b) => hits.filter((h) => h.c.table === b).length - hits.filter((h) => h.c.table === a).length);
    for (const t of tables) {
      const mine = hits.filter((h) => h.c.table === t);
      const lane = el("div", {
        class: "spec-lane"
      });
      for (const [cos] of marks) {
        lane.append(el("i", {
          class: "spec-rule",
          style: `left:${xOf(cos)}%`
        }));
      }
      const placed = [];
      for (const h of mine) {
        const x = xOf(h.cos);
        const row = placed.filter((p) => Math.abs(p - x) < 2.2).length;
        placed.push(x);
        const dot = el("button", {
          class: "spec-dot" + (h.c.selected ? " sel" : ""),
          style: `left:${x}%; top:${8 + Math.min(row, 3) * 9}px; --th:${hueOf(h.c.table)}`
        });
        hov(dot, `<div class="hc-head"><span class="hc-ref">for \u201C${esc(h.sp.text)}\u201D</span><span class="hc-score">cos ${h.cos.toFixed(3)}</span></div>` + hovCandidate(h.c));
        dot.addEventListener("click", (e) => {
          e.stopPropagation();
          showCard(h.sp, h.c);
        });
        lane.append(dot);
      }
      const bestMine = mine[0];
      spec.append(el("div", {
        class: "spec-row"
      }, el("span", {
        class: "spec-lab"
      }, el("b", {
        style: `color:${hueOf(t)}`
      }, t), el("i", null, `${mine.length} \xB7 best ${bestMine.cos.toFixed(2)}`)), lane));
    }
    spec.append(el("div", {
      class: "spec-axis"
    }, el("span", {
      class: "spec-end"
    }, "\u2190 noise"), el("span", {
      class: "spec-end spec-near"
    }, "nearer the query \u2192")));
    host.append(spec, el("div", {
      class: "sql-caption",
      style: "margin-top:6px"
    }, "accent dots were selected as mentions \xB7 grey dots are the retrieved-but-outranked field \xB7 click any dot for its card"));
  }
  for (const [k, p] of Object.entries(panels)) p.node.hidden = k !== mode;
  if (!panels[mode].built) {
    panels[mode].build();
    panels[mode].built = true;
  }
  out.append(el("div", {
    class: "sql-caption"
  }, `resolved in ${trace.elapsed_ms.toFixed(1)} ms \xB7 ${trace.spans.length} spans enumerated \xB7 channels: exact, bm25, trigram, dense, kg`), lineage, modeBar, panels.anatomy.node, panels.space.node, card, alsoBox);
}
var MAX_LAT_N = 4;
function renderMiniTrace(trace) {
  const box = el("div", {
    class: "minitraj"
  });
  const mentionSpans = trace.mentions.map((i) => trace.spans[i]);
  const covered = (pos) => mentionSpans.some((s) => pos >= s.start && pos < s.end);
  const qline = el("div", {
    class: "mini-qline"
  });
  let cursor = 0;
  for (const t of trace.tokens) {
    if (t.start > cursor) qline.append(trace.query.slice(cursor, t.start));
    qline.append(el("span", {
      class: "qtok" + (covered(t.start) ? " mention" : t.stopword ? " stop" : "")
    }, t.text));
    cursor = t.end;
  }
  if (cursor < trace.query.length) qline.append(trace.query.slice(cursor));
  box.append(qline);
  for (const sp of mentionSpans) {
    const sel = sp.candidates.filter((c) => c.selected);
    const missed = sp.candidates.length - sel.length;
    const lane = el("div", {
      class: "mini-lane"
    }, el("div", {
      class: "mini-span"
    }, sp.text));
    for (const c of sel.slice(0, 3)) {
      lane.append(el("div", {
        class: "mini-cand"
      }, el("span", {
        class: "mini-ref"
      }, `${c.table}.${c.column} #${c.rowid}`), c.is_doc && c.snippet ? snippetNode(c.snippet) : el("span", {
        class: "mini-val"
      }, `\u201C${c.value}\u201D`), el("span", {
        class: "meter"
      }, el("span", {
        style: `width:${Math.round(c.score * 100)}%`
      }))));
    }
    if (sel.length > 3 || missed > 0) {
      lane.append(el("div", {
        class: "mini-more"
      }, [
        sel.length > 3 ? `+${sel.length - 3} more` : "",
        missed > 0 ? `${missed} near-miss${missed === 1 ? "" : "es"}` : ""
      ].filter(Boolean).join(" \xB7 ")));
    }
    box.append(lane);
  }
  if (!mentionSpans.length) {
    box.append(el("div", {
      class: "empty"
    }, "\u2014 nothing resolved"));
  }
  return box;
}
var pendingTrace = null;
function showTraceInMain(trace) {
  pendingTrace = trace;
  const target = "#/query?d=nl&q=" + encodeURIComponent(trace.query);
  if (location.hash === target) route();
  else location.hash = target;
}
function chatRailOpen() {
  return localStorage.getItem("stemma.chatrail") === "open";
}
function setChatRail(open) {
  localStorage.setItem("stemma.chatrail", open ? "open" : "closed");
  const rail = document.getElementById("chatrail");
  const grid = document.getElementById("bodygrid");
  const btn = document.getElementById("chattoggle");
  const savedW = localStorage.getItem("stemma.railw");
  if (savedW) grid.style.setProperty("--railw", savedW);
  rail.hidden = !open;
  grid.classList.toggle("chat-open", open);
  btn.classList.toggle("accent", open);
  hideHover();
  if (open) renderChatRail();
}
function unwrapToolResult(result) {
  if (result == null || typeof result !== "object") {
    if (typeof result === "string") {
      try {
        return JSON.parse(result);
      } catch {
        return result;
      }
    }
    return result;
  }
  const r = result;
  const sc = r.structuredContent;
  if (sc) return sc.result ?? sc;
  const content = r.content;
  if (Array.isArray(content)) {
    const text = content.find((c) => c.type === "text")?.text;
    if (text != null) {
      try {
        return JSON.parse(text);
      } catch {
        return text;
      }
    }
  }
  return result;
}
function toolResult(tool, raw, args) {
  const r = unwrapToolResult(raw);
  if (r == null) return el("div", {
    class: "tool-body"
  }, "\u2014");
  if (typeof r === "object") {
    const o = r;
    if (tool === "sql" && Array.isArray(o.columns)) return sqlResult(o);
    if (tool === "knowledge_graph" && (o.tables || o.characteristic_terms)) return kgResult(o);
    if (tool === "schema" && Array.isArray(o.tables)) return schemaResult(o);
  }
  return el("div", {
    class: "tool-body"
  }, typeof r === "string" ? r : JSON.stringify(r, null, 2));
}
function sqlResult(o) {
  const cols = o.columns;
  const rows = o.rows ?? [];
  const box = el("div", {
    class: "tr-box"
  });
  if (!rows.length) {
    box.append(el("div", {
      class: "tr-note"
    }, "no rows"));
    return box;
  }
  const numeric = cols.map((_, i) => rows.every((r) => typeof r[i] === "number" || r[i] == null));
  const table = el("table", {
    class: "tr-table"
  }, el("thead", null, el("tr", null, cols.map((c, i) => el("th", {
    class: numeric[i] ? "num" : null
  }, c)))), el("tbody", null, rows.map((r) => el("tr", null, r.map((v, i) => {
    const s = v == null ? "\u2205" : String(v);
    return el("td", {
      class: numeric[i] ? "num" : null,
      title: s.length > 80 ? s : null
    }, s.length > 80 ? s.slice(0, 80) + "\u2026" : s);
  })))));
  box.append(el("div", {
    class: "tr-scroll"
  }, table));
  if (o.truncated) box.append(el("div", {
    class: "tr-note"
  }, "truncated \u2014 showing the first rows"));
  return box;
}
function kgResult(o) {
  const box = el("div", {
    class: "tr-box"
  });
  const tables = o.tables ?? [];
  if (tables.length) {
    box.append(el("div", {
      class: "tr-sub"
    }, "tables"), el("div", {
      class: "tr-chips"
    }, tables.map((t) => el("span", {
      class: "chip tr-tbl"
    }, String(t.name), t.rows != null || t.approx_rows != null ? el("i", null, ` ~${Number(t.rows ?? t.approx_rows).toLocaleString()}`) : null))));
  }
  const terms = o.characteristic_terms ?? [];
  if (terms.length) {
    box.append(el("div", {
      class: "tr-sub"
    }, "characteristic terms"), el("div", {
      class: "tr-terms"
    }, terms.slice(0, 24).map((t, i) => el("span", {
      class: "tr-term tr-t" + (i < 4 ? 0 : i < 10 ? 1 : 2)
    }, t))));
  }
  const joins = o.joins ?? [];
  if (joins.length) {
    box.append(el("div", {
      class: "tr-sub"
    }, "join paths"), el("div", null, joins.map((j) => el("div", {
      class: "tr-join"
    }, el("b", null, String(j.from).replace(/^table:/, "")), el("span", {
      class: "tr-arrow"
    }, ` \u2014${j.label ?? ""}\u2192 `), el("b", null, String(j.to).replace(/^table:/, "")), j.confidence != null ? el("span", {
      class: "tr-conf"
    }, ` ${j.method === "inferred" || j.method === "inclusion" ? "inferred \xB7 " : ""}${Number(j.confidence).toFixed(2)}`) : null))));
  }
  if (!tables.length && !terms.length && !joins.length) {
    box.append(el("div", {
      class: "tr-note"
    }, "empty graph"));
  }
  return box;
}
function schemaResult(o) {
  const box = el("div", {
    class: "tr-box"
  });
  for (const t of o.tables) {
    box.append(el("div", {
      class: "tr-schema"
    }, el("b", null, String(t.name)), el("span", {
      class: "tr-conf"
    }, ` ~${Number(t.approx_rows ?? 0).toLocaleString()} \xB7 `), el("span", {
      class: "tr-cols"
    }, (t.columns ?? []).join(", ")), ...(t.foreign_keys ?? []).map((fk) => el("div", {
      class: "tr-join tr-fk"
    }, `\u21B3 ${fk}`))));
  }
  return box;
}
function renderChatRail() {
  const rail = document.getElementById("chatrail");
  rail.replaceChildren();
  const widthGrip = el("div", {
    class: "rail-resize",
    title: "drag to resize"
  }, el("i"));
  widthGrip.addEventListener("pointerdown", (down) => {
    down.preventDefault();
    widthGrip.setPointerCapture(down.pointerId);
    const grid = document.getElementById("bodygrid");
    const move = (e) => {
      const w = Math.round(Math.min(720, Math.max(300, document.documentElement.clientWidth - e.clientX)));
      grid.style.setProperty("--railw", `${w}px`);
    };
    widthGrip.addEventListener("pointermove", move);
    widthGrip.addEventListener("pointerup", () => {
      widthGrip.removeEventListener("pointermove", move);
      localStorage.setItem("stemma.railw", grid.style.getPropertyValue("--railw") || "380px");
    }, {
      once: true
    });
  });
  rail.append(widthGrip);
  const db = state.db;
  const conv = activeConv(db);
  const key = `${db}:${conv}`;
  const convPick = el("select", {
    class: "input rail-convpick",
    onchange: () => {
      setActiveConv(db, convPick.value);
      renderChatRail();
    }
  });
  const newBtn = el("button", {
    class: "btn accent",
    title: "start a new chat",
    onclick: () => {
      const id = "c" + Date.now().toString(36);
      setActiveConv(db, id);
      chatLog.set(`${db}:${id}`, []);
      renderChatRail();
    }
  }, "+ new chat");
  rail.append(el("div", {
    class: "rail-head"
  }, el("span", {
    class: "subhead",
    style: "margin:0"
  }, "chat"), el("span", {
    class: "sql-caption"
  }, state.cfg?.lm ? `${db} \xB7 ${state.cfg.lm.model}` : "no model configured"), el("span", {
    class: "spacer"
  }), newBtn));
  rail.append(el("div", {
    class: "rail-convrow"
  }, convPick));
  getJSON(`/api/db/${db}/chats`).then((r) => {
    const seen = /* @__PURE__ */ new Set();
    convPick.replaceChildren();
    for (const c of r.conversations) {
      seen.add(c.id);
      convPick.append(el("option", {
        value: c.id,
        selected: c.id === conv ? "" : null
      }, `${c.title || c.id} \xB7 ${Math.ceil(c.turns / 2)} turns`));
    }
    if (!seen.has(conv)) {
      convPick.append(el("option", {
        value: conv,
        selected: ""
      }, "(new chat)"));
    }
  }).catch(() => {
  });
  if (!state.cfg?.lm) {
    rail.append(el("div", {
      class: "rail-transcript"
    }, el("div", {
      class: "empty"
    }, "\u2014 talk to the data by proxy needs a model: set console.lm in config.json (endpoint, model, api_key) or restart the console with --lm-endpoint http://host:port/v1 --lm-model <name> (any openai-compatible server: vllm, llama.cpp, litellm)")));
    return;
  }
  if (!chatLog.has(key)) {
    chatLog.set(key, []);
    getJSON(`/api/db/${db}/chat?conversation=${encodeURIComponent(conv)}`).then((r) => {
      const cur = chatLog.get(key);
      if (cur.length === 0 && r.messages.length) {
        cur.push(...r.messages);
        if (chatRailOpen()) renderChatRail();
      }
    }).catch(() => {
    });
  }
  const log = chatLog.get(key);
  const transcript = el("div", {
    class: "rail-transcript"
  });
  const input = el("textarea", {
    class: "input rail-chatinput",
    rows: "1",
    placeholder: `ask ${db} anything\u2026`,
    onkeydown: (e) => {
      const k = e;
      if (k.key === "Enter" && !k.shiftKey) {
        k.preventDefault();
        send();
      }
    }
  });
  const savedH = localStorage.getItem("stemma.chatinputh");
  if (savedH) input.style.height = savedH;
  const sendBtn = el("button", {
    class: "btn accent",
    onclick: () => send()
  }, "send");
  const heightGrip = el("div", {
    class: "rail-inputgrip",
    title: "drag to resize"
  }, el("i"));
  heightGrip.addEventListener("pointerdown", (down) => {
    down.preventDefault();
    heightGrip.setPointerCapture(down.pointerId);
    const bottom = input.getBoundingClientRect().bottom;
    const move = (e) => {
      const h = Math.round(Math.min(window.innerHeight * 0.4, Math.max(34, bottom - e.clientY)));
      input.style.height = `${h}px`;
    };
    heightGrip.addEventListener("pointermove", move);
    heightGrip.addEventListener("pointerup", () => {
      heightGrip.removeEventListener("pointermove", move);
      localStorage.setItem("stemma.chatinputh", input.style.height);
    }, {
      once: true
    });
  });
  rail.append(transcript, heightGrip, el("div", {
    class: "rail-inputrow"
  }, input, sendBtn));
  redraw();
  function redraw() {
    transcript.replaceChildren();
    if (!log.length) {
      transcript.append(el("div", {
        class: "empty"
      }, "\u2014 every mention the model uses is pinned through resolve first; tool calls appear here, trajectories open in the main view"));
    }
    for (const m of log) {
      if (m.role === "user") {
        transcript.append(el("div", {
          class: "chat-msg user"
        }, el("div", {
          class: "who"
        }, "you"), el("div", {
          class: "md"
        }, m.content)));
      } else {
        for (const t of m.trail ?? []) transcript.append(renderTrailItem(t));
        transcript.append(el("div", {
          class: "chat-msg"
        }, el("div", {
          class: "who"
        }, "stemma"), md(m.content)));
      }
    }
    transcript.scrollTop = transcript.scrollHeight;
  }
  function renderTrailItem(t) {
    if (t.tool === "resolve" && t.trace) {
      const trace = t.trace;
      const d2 = el("details", {
        class: "chat-tool",
        open: ""
      });
      d2.append(el("summary", null, el("span", {
        class: "chip"
      }, "resolve"), `\u201C${trace.query}\u201D \xB7 ${trace.mentions.length} mention${trace.mentions.length === 1 ? "" : "s"}`));
      d2.append(renderMiniTrace(trace));
      d2.append(el("div", {
        style: "margin:3px 0 2px"
      }, el("button", {
        class: "rail-showtraj",
        onclick: () => showTraceInMain(trace)
      }, "full trajectory \u2192")));
      return d2;
    }
    const d = el("details", {
      class: "chat-tool"
    });
    const label = t.tool === "sql" ? `sql \xB7 ${(t.args.query ?? "").slice(0, 60)}` : t.tool;
    d.append(el("summary", null, el("span", {
      class: "chip"
    }, t.tool), label));
    d.append(toolResult(t.tool, t.result, t.args));
    return d;
  }
  async function send() {
    const text = input.value.trim();
    if (!text) return;
    input.value = "";
    log.push({
      role: "user",
      content: text
    });
    redraw();
    const wait = el("div", {
      class: "chat-wait"
    }, el("i"), el("i"), el("i"));
    transcript.append(wait);
    sendBtn.setAttribute("disabled", "");
    try {
      const r = await fetch(`/api/db/${db}/chat`, {
        method: "POST",
        headers: {
          "content-type": "application/json"
        },
        body: JSON.stringify({
          conversation: conv,
          messages: log.map((m) => ({
            role: m.role,
            content: m.content
          }))
        })
      });
      const d = await r.json();
      if (!r.ok) throw new Error(d.detail ?? r.statusText);
      log.push({
        role: "assistant",
        content: d.message,
        trail: d.trail
      });
      const lastResolve = [
        ...d.trail
      ].reverse().find((t) => t.tool === "resolve" && t.trace);
      if (lastResolve?.trace) showTraceInMain(lastResolve.trace);
    } catch (e) {
      log.push({
        role: "assistant",
        content: "\u2014 " + e.message,
        trail: []
      });
    } finally {
      wait.remove();
      sendBtn.removeAttribute("disabled");
      redraw();
    }
  }
}
async function viewData(host, params, table) {
  const tables = state.schema?.tables ?? [];
  const name = table ?? tables[0]?.name;
  setCrumbs("data", name);
  if (!name) {
    host.append(el("div", {
      class: "empty"
    }, "\u2014 no tables in this database"));
    return;
  }
  const limit = 50;
  const meta = tables.find((t) => t.name === name);
  const afterParam = params.get("after");
  const cursors = [
    afterParam !== null ? Number(afterParam) : null
  ];
  let lastRowid = null;
  let hasMore = false;
  let filter = params.get("q") ?? "";
  const filterInput = el("input", {
    class: "input",
    value: filter,
    placeholder: "filter \u2014 substring across text columns (trigram-served)\u2026",
    onkeydown: (e) => {
      if (e.key === "Enter") {
        filter = filterInput.value.trim();
        cursors.length = 1;
        load(null);
      }
    }
  });
  const body = el("div", null);
  host.append(el("h1", {
    class: "h1"
  }, name), el("p", {
    class: "sql-caption"
  }, meta ? `~${meta.row_count.toLocaleString()} rows \xB7 ` + meta.columns.map((c) => `${c.name} ${c.type.toLowerCase()}${c.pk ? " \xB7pk" : ""}`).join(" \xB7 ") : ""), el("div", {
    class: "data-tools"
  }, filterInput, el("button", {
    class: "btn",
    onclick: () => {
      filter = filterInput.value.trim();
      cursors.length = 1;
      load(null);
    }
  }, "filter")), body);
  await load(cursors[0]);
  async function load(after) {
    body.replaceChildren(el("div", {
      class: "empty"
    }, "loading\u2026"));
    const qs = new URLSearchParams({
      limit: String(limit)
    });
    if (after !== null) qs.set("after", String(after));
    if (filter) qs.set("q", filter);
    const d = await getJSON(`/api/db/${state.db}/rows/${encodeURIComponent(name)}?${qs}`);
    hasMore = d.has_more;
    const ridIdx = d.columns.indexOf("_rowid");
    lastRowid = d.rows.length ? Number(d.rows[d.rows.length - 1][ridIdx]) : null;
    const tbl = el("table", {
      class: "grid"
    }, el("thead", null, el("tr", null, d.columns.map((c) => el("th", null, c)))), el("tbody", null, d.rows.map((r) => el("tr", null, r.map((v) => el("td", {
      class: typeof v === "number" ? "num" : null
    }, v === null ? "\u2205" : v))))));
    const pager = el("div", {
      class: "pager"
    }, el("button", {
      class: "btn",
      disabled: cursors.length <= 1 ? "" : null,
      onclick: () => {
        cursors.pop();
        load(cursors[cursors.length - 1]);
      }
    }, "\u2039 prev"), el("button", {
      class: "btn",
      disabled: hasMore ? null : "",
      onclick: () => {
        cursors.push(lastRowid);
        load(lastRowid);
      }
    }, "next \u203A"), el("span", {
      class: "where"
    }, d.rows.length ? `page ${cursors.length} \xB7 ${d.rows.length} rows${filter ? ` \xB7 filtered \u201C${filter}\u201D` : ""}` : "\u2014 nothing matches"));
    body.replaceChildren(el("div", {
      class: "table-scroll"
    }, tbl), pager);
  }
}
async function viewGraph(host) {
  setCrumbs("graph");
  const g = await getJSON(`/api/db/${state.db}/graph`);
  host.append(el("h1", {
    class: "h1"
  }, "knowledge graph"), el("p", {
    class: "lede"
  }, g.layer === "compiled" ? "two readings of the compiled graph: the map (typographic, scannable) and the diagram (spatial, force-laid). join paths \u2014 including transitive routes through intermediate tables \u2014 are computed below; click one to light the route." : "schema layer only \u2014 run stemma-server against this database once to compile the full graph."));
  if (!g.nodes.length) {
    host.append(el("div", {
      class: "empty"
    }, "\u2014 nothing compiled"));
    return;
  }
  const byKey = new Map(g.nodes.map((n) => [
    n.key,
    n
  ]));
  const touching = (key) => g.edges.filter((e) => e.source === key || e.target === key);
  const cent = (n) => Number(n.props.centrality ?? 0);
  const maxCent = Math.max(1e-6, ...g.nodes.map(cent));
  const tables = g.nodes.filter((n) => n.kind === "table");
  const joinEdges = g.edges.filter((e) => e.kind === "fk" || e.kind === "inferred_fk");
  const joinPaths = [];
  {
    const seen = /* @__PURE__ */ new Set();
    const walk = (at, path, visited) => {
      if (path.length > 0) {
        const sig = path.map((s) => `${s.from}>${s.to}:${s.edge.label}`).join("|");
        const rsig = [
          ...path
        ].reverse().map((s) => `${s.to}>${s.from}:${s.edge.label}`).join("|");
        if (!seen.has(sig) && !seen.has(rsig)) {
          seen.add(sig);
          joinPaths.push([
            ...path
          ]);
        }
      }
      if (path.length >= 3) return;
      for (const e of joinEdges) {
        const nxt = e.source === at ? e.target : e.target === at ? e.source : null;
        if (!nxt || visited.has(nxt)) continue;
        visited.add(nxt);
        path.push({
          edge: e,
          from: at,
          to: nxt
        });
        walk(nxt, path, visited);
        path.pop();
        visited.delete(nxt);
      }
    };
    for (const t of tables) walk(t.key, [], /* @__PURE__ */ new Set([
      t.key
    ]));
  }
  joinPaths.sort((a, b) => a.length - b.length);
  const mode = {
    v: localStorage.getItem("stemma.graphmode") ?? "map"
  };
  const modeSeg = el("span", {
    class: "seg"
  }, [
    "map",
    "diagram"
  ].map((m) => el("button", {
    class: mode.v === m ? "on" : "",
    onclick: () => {
      mode.v = m;
      localStorage.setItem("stemma.graphmode", m);
      modeSeg.querySelectorAll("button").forEach((b, i) => b.classList.toggle("on", [
        "map",
        "diagram"
      ][i] === m));
      render();
    }
  }, m)));
  const shown = /* @__PURE__ */ new Set([
    "column",
    "value",
    "term"
  ]);
  const legend = el("div", {
    class: "graph-legend"
  }, modeSeg);
  for (const k of [
    "column",
    "value",
    "term"
  ]) {
    const count = g.nodes.filter((x) => x.kind === k).length;
    if (!count) continue;
    const chip = el("button", {
      class: "chip",
      onclick: () => {
        if (shown.has(k)) shown.delete(k);
        else shown.add(k);
        chip.classList.toggle("off");
        render();
      }
    }, `${k}s \xB7 ${count}`);
    legend.append(chip);
  }
  const searchBox = el("input", {
    class: "input kg-search",
    placeholder: "find in graph\u2026",
    oninput: () => {
      const q = searchBox.value.trim().toLowerCase();
      for (const [k, elm] of labelEls) {
        const n = byKey.get(k);
        const hit = q !== "" && (n?.label ?? "").toLowerCase().includes(q);
        elm.classList.toggle("kg-hit", hit);
        elm.classList.toggle("kg-dim", q !== "" && !hit);
      }
    }
  });
  const zoomSeg = el("span", {
    class: "seg",
    style: "margin-left:auto"
  }, el("button", {
    onclick: () => zoomBy(1 / 1.25)
  }, "\u2212"), el("button", {
    onclick: () => fit()
  }, "fit"), el("button", {
    onclick: () => zoomBy(1.25)
  }, "+"));
  legend.append(searchBox, zoomSeg);
  const pathStrip = el("div", {
    class: "joinpaths"
  }, el("span", {
    class: "subhead",
    style: "margin:0 10px 0 0"
  }, `join paths \xB7 ${joinPaths.length}`));
  if (!joinPaths.length) {
    pathStrip.append(el("span", {
      class: "empty",
      style: "padding:0"
    }, "\u2014 no joins declared or discovered between tables"));
  }
  let activePath = null;
  for (const path of joinPaths.slice(0, 14)) {
    const chainText = [
      byKey.get(path[0].from)?.label ?? "",
      ...path.map((st) => byKey.get(st.to)?.label ?? "")
    ].join(" \u2192 ");
    const inferred = path.some((st) => st.edge.kind === "inferred_fk");
    const chipEl = el("button", {
      class: "chip joinpath" + (inferred ? " inferred" : ""),
      onclick: (e) => {
        e.stopPropagation();
        activePath = activePath === path ? null : path;
        pathStrip.querySelectorAll(".joinpath").forEach((x) => x.classList.remove("on-chan"));
        if (activePath) chipEl.classList.add("on-chan");
        highlightPath();
      }
    }, chainText + (path.length > 1 ? ` \xB7 ${path.length} hops` : ""));
    hov(chipEl, path.map((st) => `<b>${esc(byKey.get(st.from)?.label ?? "")} \u2192 ${esc(byKey.get(st.to)?.label ?? "")}</b> ${esc(st.edge.label)} \xB7 ${esc(String(st.edge.props.method ?? ""))}` + (st.edge.kind === "inferred_fk" ? ` \xB7 confidence ${st.edge.props.confidence ?? "?"}` : "")).join("<br>"));
    pathStrip.append(chipEl);
  }
  const detail = el("div", {
    class: "graph-detail",
    hidden: ""
  });
  const canvas = el("div", {
    class: "kg-canvas"
  });
  const viewport = el("div", {
    class: "kg-viewport"
  }, canvas);
  host.append(legend, pathStrip, detail, viewport);
  let scale = 1, tx = 0, ty = 0;
  const DIAG_W = 1400, DIAG_H = 1e3;
  function applyTransform() {
    canvas.style.transform = `translate(${tx}px, ${ty}px) scale(${scale})`;
    canvas.classList.toggle("kg-zoomed-out", scale < 0.75);
  }
  function zoomBy(f, cxv, cyv) {
    const rect = viewport.getBoundingClientRect();
    const px = cxv ?? rect.width / 2, py = cyv ?? rect.height / 2;
    const ns = Math.min(3, Math.max(0.3, scale * f));
    tx = px - (px - tx) / scale * ns;
    ty = py - (py - ty) / scale * ns;
    scale = ns;
    applyTransform();
  }
  function fit() {
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
  }, {
    passive: false
  });
  let drag = null;
  viewport.addEventListener("pointerdown", (e) => {
    if (e.target.closest(".gnode, .kg-label, .kg-tablebox, button")) return;
    drag = {
      x: e.clientX,
      y: e.clientY,
      tx,
      ty
    };
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
  let selectedKey = null;
  const labelEls = /* @__PURE__ */ new Map();
  let edgeEls = [];
  let mapWires = null;
  function nodeRadius(n) {
    if (n.kind === "table") return 30;
    if (n.kind === "column") return 7;
    const c = Math.sqrt(cent(n) / maxCent);
    return n.key.startsWith("phrase:") ? 6 + c * 6 : 4 + c * 9;
  }
  function nodeColor(n) {
    if (n.kind === "table") return "var(--ink)";
    if (n.kind === "column") return "var(--flat)";
    if (n.kind === "value") return "var(--caution)";
    if (n.key.startsWith("phrase:")) return "var(--good)";
    const mix = Math.min(85, Math.round(Math.sqrt(cent(n) / maxCent) * 85));
    return `color-mix(in srgb, var(--flat) ${100 - mix}%, var(--accent) ${mix}%)`;
  }
  function render() {
    hideHover();
    labelEls.clear();
    edgeEls = [];
    mapWires = null;
    if (mode.v === "diagram") renderDiagram();
    else renderMap();
    if (selectedKey && byKey.has(selectedKey)) select(byKey.get(selectedKey), true);
    highlightPath();
    requestAnimationFrame(fit);
  }
  function renderMap() {
    const wires = svgEl("svg", {
      class: "kg-wires",
      "aria-hidden": "true"
    });
    mapWires = wires;
    const map = el("div", {
      class: "kg-map"
    });
    canvas.replaceChildren(wires, map);
    for (const t of tables) {
      const cell = el("div", {
        class: "kg-cell"
      });
      const header = el("div", {
        class: "kg-tablebox kg-label",
        onclick: (e) => {
          e.stopPropagation();
          select(t);
        }
      }, el("span", {
        class: "kg-tablename"
      }, t.label), el("span", {
        class: "kg-tablerows"
      }, `~${Number(t.props.rows ?? 0).toLocaleString()} rows`));
      labelEls.set(t.key, header);
      cell.append(header);
      const columns = g.nodes.filter((n) => n.kind === "column" && n.key.startsWith(`column:${t.label}.`));
      const terms = g.nodes.filter((n) => n.kind === "term" && n.key.startsWith(`term:${t.label}:`)).sort((a, b) => cent(b) - cent(a));
      const phrases = g.nodes.filter((n) => n.kind === "term" && n.key.startsWith(`phrase:${t.label}:`)).sort((a, b) => cent(b) - cent(a));
      const values = g.nodes.filter((n) => n.kind === "value" && n.key.startsWith(`value:${t.label}.`));
      const section = (title, cls, ns, style) => {
        if (!ns.length) return;
        cell.append(el("div", {
          class: "subhead kg-subhead"
        }, title));
        const flow = el("div", {
          class: "kg-flow"
        });
        for (const n of ns) {
          const lab = el("span", {
            class: `kg-label ${cls}`,
            style: style?.(n) ?? null,
            onclick: (e) => {
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
                if (!selectedKey || !touching(selectedKey).some((e3) => e3.source === k2 || e3.target === k2)) x.classList.remove("hood");
              });
            }
          }, n.label);
          hov(lab, `<b>${esc(n.label)}</b> \xB7 ${esc(n.kind)}<br>` + Object.entries(n.props).map(([k, v]) => `${esc(k)} ${esc(v)}`).join(" \xB7 "));
          labelEls.set(n.key, lab);
          flow.append(lab);
        }
        cell.append(flow);
      };
      if (shown.has("column")) section("columns", "kg-col", columns);
      if (shown.has("value")) section("frequent values", "kg-value", values);
      if (shown.has("term")) {
        section("characteristic terms \xB7 pagerank", "kg-term", terms, (n) => {
          const size = 10.5 + Math.min(5, Math.sqrt(cent(n)) * 26);
          const mix = Math.min(78, Math.round(Math.sqrt(cent(n) / maxCent) * 78));
          return `font-size: calc(${size.toFixed(1)}px * var(--fs)); color: color-mix(in srgb, var(--ink-soft) ${100 - mix}%, var(--accent) ${mix}%)`;
        });
        section("named entities", "kg-phrase", phrases);
      }
      map.append(cell);
    }
    requestAnimationFrame(drawMapWires);
  }
  function mapAnchor(elm) {
    const r = elm.getBoundingClientRect();
    const c = canvas.getBoundingClientRect();
    return {
      x: (r.left + r.width / 2 - c.left) / scale,
      y: (r.bottom - c.top) / scale,
      top: (r.top - c.top) / scale
    };
  }
  function drawMapWires() {
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
        d: `M ${pa.x} ${pa.y} C ${pa.x} ${pa.y + 46}, ${pb.x} ${pb.top - 46}, ${pb.x} ${pb.top}`
      });
      hov(path, `<b>${esc(e.kind)}</b> ${esc(e.label)}`);
      edgeEls.push({
        el: path,
        e
      });
      mapWires.append(path);
    }
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
            d: `M ${ps.x} ${ps.y} C ${ps.x} ${ps.y + 34}, ${po.x} ${po.top - 34}, ${po.x} ${po.top}`
          }));
        }
      }
    }
    highlightPath();
  }
  function renderDiagram() {
    const nodes = g.nodes.filter((n) => n.kind === "table" || shown.has(n.kind));
    const keys = new Set(nodes.map((n) => n.key));
    const edges = g.edges.filter((e) => keys.has(e.source) && keys.has(e.target));
    const idx = new Map(nodes.map((n, i) => [
      n.key,
      i
    ]));
    const pos = nodes.map(() => ({
      x: 0,
      y: 0,
      vx: 0,
      vy: 0
    }));
    const pinned = /* @__PURE__ */ new Set();
    tables.forEach((t, ti) => {
      const i = idx.get(t.key);
      if (i === void 0) return;
      const a = 2 * Math.PI * ti / tables.length - Math.PI / 2;
      const R = tables.length > 1 ? Math.min(DIAG_W, DIAG_H) * 0.26 : 0;
      pos[i].x = DIAG_W / 2 + R * Math.cos(a);
      pos[i].y = DIAG_H / 2 + R * Math.sin(a);
      pinned.add(i);
    });
    const GOLDEN = 2.399963;
    const childCount = /* @__PURE__ */ new Map();
    nodes.forEach((n, i) => {
      if (pinned.has(i)) return;
      const owner = edges.find((e) => e.target === n.key && byKey.get(e.source)?.kind === "table") ?? edges.find((e) => e.source === n.key && byKey.get(e.target)?.kind === "table");
      const ownerKey = owner ? byKey.get(owner.source)?.kind === "table" ? owner.source : owner.target : tables[0]?.key;
      const oi = ownerKey !== void 0 ? idx.get(ownerKey) ?? 0 : 0;
      const k = (childCount.get(oi) ?? 0) + 1;
      childCount.set(oi, k);
      const r = 60 + 14 * Math.sqrt(k);
      pos[i].x = pos[oi].x + r * Math.cos(k * GOLDEN);
      pos[i].y = pos[oi].y + r * Math.sin(k * GOLDEN);
    });
    const collide = nodes.map((n) => Math.max(nodeRadius(n) + 6, n.label.length * 2.6 + 6));
    const rest = (e) => e.kind === "fk" || e.kind === "inferred_fk" ? 420 : e.kind === "has_column" ? 110 : e.kind === "cooccurs" ? 150 : 170;
    for (let it = 0; it < 220; it++) {
      for (let i = 0; i < nodes.length; i++) {
        for (let j = i + 1; j < nodes.length; j++) {
          let dx = pos[j].x - pos[i].x, dy = pos[j].y - pos[i].y;
          let d2 = dx * dx + dy * dy;
          if (d2 < 1) {
            dx = (i * 7 + j) % 13 - 6;
            dy = (i * 5 + j) % 11 - 5;
            d2 = dx * dx + dy * dy;
          }
          const d = Math.sqrt(d2);
          const minD = collide[i] + collide[j];
          let f = 900 / d2;
          if (d < minD) f += (minD - d) * 0.06;
          const fx = dx / d * f, fy = dy / d * f;
          if (!pinned.has(i)) {
            pos[i].vx -= fx;
            pos[i].vy -= fy;
          }
          if (!pinned.has(j)) {
            pos[j].vx += fx;
            pos[j].vy += fy;
          }
        }
      }
      for (const e of edges) {
        const a = idx.get(e.source), b = idx.get(e.target);
        const dx = pos[b].x - pos[a].x, dy = pos[b].y - pos[a].y;
        const d = Math.max(1, Math.hypot(dx, dy));
        const f = (d - rest(e)) * 0.015;
        const fx = dx / d * f, fy = dy / d * f;
        if (!pinned.has(a)) {
          pos[a].vx += fx;
          pos[a].vy += fy;
        }
        if (!pinned.has(b)) {
          pos[b].vx -= fx;
          pos[b].vy -= fy;
        }
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
      class: "graph-svg",
      viewBox: `0 0 ${DIAG_W} ${DIAG_H}`,
      width: DIAG_W,
      height: DIAG_H,
      role: "img",
      "aria-label": "knowledge graph"
    });
    svg.append(svgEl("defs", null, svgEl("marker", {
      id: "arrow",
      viewBox: "0 0 8 8",
      refX: 7,
      refY: 4,
      markerWidth: 6,
      markerHeight: 6,
      orient: "auto"
    }, svgEl("path", {
      d: "M 0 0 L 8 4 L 0 8 z",
      fill: "var(--flat)"
    }))));
    for (const e of edges) {
      const a = pos[idx.get(e.source)], b = pos[idx.get(e.target)];
      const mx = (a.x + b.x) / 2 + (a.y - b.y) * 0.06;
      const my = (a.y + b.y) / 2 + (b.x - a.x) * 0.06;
      const path = svgEl("path", {
        class: `gedge kind-${e.kind}`,
        d: `M ${a.x} ${a.y} Q ${mx} ${my} ${b.x} ${b.y}`,
        ...e.kind === "fk" || e.kind === "inferred_fk" ? {
          "marker-end": "url(#arrow)"
        } : {}
      });
      if (e.label) {
        hov(path, `<b>${esc(e.kind)}</b> ${esc(e.label)}`);
      }
      edgeEls.push({
        el: path,
        e
      });
      svg.append(path);
      if (e.kind === "fk" || e.kind === "inferred_fk") {
        svg.append(svgEl("text", {
          class: "gedge-label",
          x: mx,
          y: my,
          "text-anchor": "middle"
        }, e.label));
      }
    }
    nodes.forEach((n, i) => {
      const p = pos[i];
      const r = nodeRadius(n);
      const grp = svgEl("g", {
        class: `gnode kind-${n.kind}` + (n.key.startsWith("phrase:") ? " is-phrase" : ""),
        transform: `translate(${p.x}, ${p.y})`,
        cursor: "pointer"
      });
      grp.append(svgEl("circle", {
        r,
        fill: nodeColor(n),
        class: "gdot"
      }));
      if (n.kind === "table") {
        grp.append(svgEl("text", {
          class: "glabel glabel-table",
          y: 4,
          "text-anchor": "middle"
        }, n.label), svgEl("text", {
          class: "grows",
          y: r + 14,
          "text-anchor": "middle"
        }, `~${Number(n.props.rows ?? 0).toLocaleString()} rows`));
      } else {
        grp.append(svgEl("text", {
          class: "glabel" + (r < 7 ? " glabel-small" : ""),
          y: r + 11,
          "text-anchor": "middle"
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
          if (!selectedKey || !touching(selectedKey).some((e3) => e3.source === k2 || e3.target === k2)) x.classList.remove("hood");
        });
      });
      hov(grp, `<b>${esc(n.label)}</b> \xB7 ${esc(n.kind)}<br>` + Object.entries(n.props).map(([k, v]) => `${esc(k)} ${esc(v)}`).join(" \xB7 "));
      labelEls.set(n.key, grp);
      svg.append(grp);
    });
    canvas.replaceChildren(svg);
  }
  function highlightPath() {
    edgeEls.forEach(({ el: x }) => x.classList.remove("path-hot"));
    labelEls.forEach((x) => x.classList.remove("path-hood"));
    if (!activePath) return;
    const involvedTables = /* @__PURE__ */ new Set([
      activePath[0].from
    ]);
    for (const st of activePath) involvedTables.add(st.to);
    for (const k of involvedTables) labelEls.get(k)?.classList.add("path-hood");
    for (const st of activePath) {
      for (const { el: pe, e } of edgeEls) {
        if (e === st.edge) pe.classList.add("path-hot");
      }
    }
  }
  function select(n, keep = false) {
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
    detail.replaceChildren(el("span", {
      class: "kindtag"
    }, n.kind), el("span", {
      class: "name"
    }, n.label), el("span", {
      class: "props"
    }, Object.entries(n.props).map(([k, v]) => `${k} ${v}`).join(" \xB7 ") || "\u2014"), el("span", {
      class: "props"
    }, `${around.length} edge${around.length === 1 ? "" : "s"}`));
    if (n.kind === "table") {
      detail.append(el("button", {
        class: "btn accent",
        onclick: () => {
          location.hash = "#/data/" + encodeURIComponent(n.label);
        }
      }, "browse data \u2192"));
    } else if (n.kind === "term" || n.kind === "value") {
      detail.append(el("button", {
        class: "btn accent",
        onclick: () => {
          location.hash = "#/query?d=nl&q=" + encodeURIComponent(n.label);
        }
      }, `resolve \u201C${n.label}\u201D \u2192`));
    }
    if (mode.v === "map") drawMapWires();
  }
  render();
}
async function route() {
  const hash = location.hash || "#/query";
  const [path, qs] = hash.slice(2).split("?");
  const [view, arg] = path.split("/");
  const params = new URLSearchParams(qs ?? "");
  hideHover();
  closeAllMenus();
  if (view === "chat") {
    setChatRail(true);
    location.hash = "#/query";
    return;
  }
  const mapped = view === "resolve" ? "query" : view === "sql" ? "query" : view || "query";
  if (view === "sql") params.set("d", "sql");
  state.view = mapped;
  document.querySelectorAll("#nav a").forEach((a) => a.classList.toggle("on", a.dataset.view === state.view));
  if (!state.schema && state.db) {
    try {
      state.schema = await getJSON(`/api/db/${state.db}/schema`);
    } catch (_e) {
      state.schema = {
        tables: []
      };
    }
  }
  renderSidebar();
  const host = document.getElementById("view");
  host.replaceChildren();
  try {
    if (state.view === "data") await viewData(host, params, arg ? decodeURIComponent(arg) : void 0);
    else if (state.view === "graph") await viewGraph(host);
    else viewQuery(host, params);
  } catch (e) {
    host.append(el("div", {
      class: "sql-error"
    }, e.message));
  }
}
(async function boot() {
  initPickers();
  const cfg = await getJSON("/api/config");
  state.cfg = cfg;
  state.dbs = cfg.databases;
  state.db = cfg.databases[0] ?? null;
  document.getElementById("topsearch").addEventListener("keydown", (ev) => {
    const e = ev;
    const target = ev.target;
    if (e.key === "Enter" && target.value.trim()) {
      location.hash = "#/query?d=nl&q=" + encodeURIComponent(target.value);
    }
  });
  document.getElementById("chattoggle").addEventListener("click", () => setChatRail(Boolean(document.getElementById("chatrail").hidden)));
  if (chatRailOpen()) setChatRail(true);
  document.querySelectorAll("#nav a").forEach((a) => a.addEventListener("click", () => {
    if (a.getAttribute("href") === location.hash) route();
  }));
  globalThis.addEventListener("hashchange", route);
  pollHealth();
  route();
})();
