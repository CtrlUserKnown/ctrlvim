/* ctrlvim landing page — the site is the editor.
 *
 * The page is one session: a tabline of buffers, a viewport with a block
 * cursor and a number gutter, a statusline, and a cmdline that doubles as the
 * fuzzy command palette. Keys go through a small normal-mode dispatcher, so
 * the page answers to the same motions ctrlvim does.
 *
 * Without JS the buffers just stack and scroll — everything below is additive.
 */

'use strict';

document.body.classList.add('js');

/* ── elements ──────────────────────────────────────────────────────────── */

const $ = (id) => document.getElementById(id);

const tabline = $('tabline');
const viewport = $('viewport');
const elMode = $('mode');
const elFname = $('fname');
const elPending = $('pending');
const elMsg = $('msg');
const elFt = $('ft');
const elPos = $('pos');
const elPct = $('pct');
const cmdline = $('cmdline');
const cmdprompt = $('prompt');
const cmdinput = $('cmdinput');
const blockcaret = $('blockcaret');
const palette = $('palette');
const help = $('help');
const helpBody = $('help-body');
const fab = $('fab');
const docq = $('docq');
const docqStatus = $('docq-status');
const docqResults = $('docq-results');

const REPO = 'https://github.com/CtrlUserKnown/ctrlvim';

/** The buffers, in tabline order, keyed by the short name used in commands. */
const bufs = Array.from(document.querySelectorAll('.buf')).map((el) => ({
  el,
  key: el.id.replace(/^buf-/, ''),
  name: el.dataset.name,
  ft: el.dataset.ft,
  icon: el.dataset.icon || '',
  lines: Array.from(el.querySelectorAll('.l')),
  cursor: 0,
}));

/* ── themes ────────────────────────────────────────────────────────────────
 * The roster ctrlvim ships (crates/ctrlvim-tui/src/theme.rs), minus the
 * terminal-following default — a web page has no host palette to follow.
 * The palettes themselves live in style.css under :root[data-theme=…].
 */

const THEMES = ['tokyonight', 'catppuccin', 'noir-cat', 'rose-pine', 'gruvbox', 'habamax'];
const THEME_NAMES = {
  tokyonight: 'Tokyo Night',
  'noir-cat': 'Noir-cat',
  'rose-pine': 'Rosé Pine',
  catppuccin: 'Catppuccin',
  gruvbox: 'Gruvbox',
  habamax: 'Habamax',
};

function setTheme(key) {
  if (!THEMES.includes(key)) return false;
  document.documentElement.dataset.theme = key;
  try { localStorage.setItem('ctrlvim.theme', key); } catch (_) { /* private mode */ }
  return true;
}

/** Accept "rose pine", "rosepine", "rose-pine" — all one theme. */
function resolveTheme(input) {
  const norm = (s) => s.toLowerCase().replace(/[^a-z]/g, '');
  const want = norm(input);
  return THEMES.find((t) => norm(t) === want)
      || THEMES.find((t) => norm(THEME_NAMES[t]) === want)
      || THEMES.find((t) => norm(t).startsWith(want));
}

/* ── state ─────────────────────────────────────────────────────────────── */

const state = {
  buf: 0,
  mode: 'NORMAL',    // NORMAL | COMMAND | SEARCH | HELP
  pending: '',       // `g` / `z` chord in flight
  number: 'abs',     // abs | rel | off
  hlsearch: true,
  pattern: '',
  matches: [],       // { line, range } per hit, in document order
  match: -1,
  pitems: [],
  pidx: 0,
  dhits: [],         // docs-search results (capped for rendering)
  dtotal: 0,         // …how many there were before the cap
  didx: 0,
};

const buf = () => bufs[state.buf];
const lines = () => buf().lines;
const cursorLine = () => lines()[buf().cursor];

const HAS_HIGHLIGHT = typeof CSS !== 'undefined' && CSS.highlights
  && typeof Highlight !== 'undefined';

/* ── block cursor ──────────────────────────────────────────────────────── */

const cursorEl = document.createElement('div');
cursorEl.id = 'cursor';

/** Leading whitespace of a line — where vim would park the cursor (`^`). */
function indentOf(line) {
  const m = /^\s*/.exec(line.textContent);
  return m ? m[0].length : 0;
}

function placeCursor() {
  const line = cursorLine();
  if (!line) return;
  const col = indentOf(line);
  cursorEl.style.left = `calc(var(--gutter) + ${col}ch)`;
  line.appendChild(cursorEl);
}

/* ── rendering ─────────────────────────────────────────────────────────── */

/** What the gutter last drew, so a `j` in a 2,400-line buffer stays cheap. */
let painted = { mode: null, buf: -1, cursor: 0 };

function gutterLabel(i, cur) {
  if (state.number === 'abs') return String(i + 1);
  if (state.number === 'rel') return i === cur ? String(i + 1) : String(Math.abs(i - cur));
  return '';
}

function paintGutter() {
  const b = buf();
  const cur = b.cursor;
  // Absolute numbers never change, so moving the cursor only moves one class.
  // Relative ones renumber the whole buffer by definition.
  const relabel = painted.mode !== state.number || painted.buf !== state.buf
    || state.number === 'rel';

  if (relabel) {
    b.lines.forEach((line, i) => {
      line.dataset.num = gutterLabel(i, cur);
      line.classList.toggle('cursorline', i === cur);
    });
  } else {
    const was = b.lines[painted.cursor];
    if (was) was.classList.remove('cursorline');
    const now = b.lines[cur];
    if (now) now.classList.add('cursorline');
  }

  painted = { mode: state.number, buf: state.buf, cursor: cur };
}

function paintStatus() {
  const b = buf();
  elMode.textContent = state.mode;
  elMode.dataset.mode = state.mode;
  elFname.textContent = b.name;
  elFt.textContent = b.ft;
  elPending.textContent = state.pending;

  const line = cursorLine();
  elPos.textContent = `${b.cursor + 1}:${line ? indentOf(line) + 1 : 1}`;

  const n = b.lines.length;
  const atBottom = viewport.scrollTop + viewport.clientHeight >= viewport.scrollHeight - 2;
  elPct.textContent = viewport.scrollHeight <= viewport.clientHeight ? 'All'
    : viewport.scrollTop <= 0 ? 'Top'
    : atBottom ? 'Bot'
    : `${Math.round((b.cursor / Math.max(1, n - 1)) * 100)}%`;
}

function render() {
  paintGutter();
  placeCursor();
  paintStatus();
}

function message(text, isError) {
  elMsg.textContent = text || '';
  elMsg.classList.toggle('err', !!isError);
}

/* ── motion ────────────────────────────────────────────────────────────── */

function moveTo(idx, opts = {}) {
  const b = buf();
  b.cursor = Math.max(0, Math.min(idx, b.lines.length - 1));
  render();
  if (opts.scroll !== false) {
    // Instant by default: smooth scrolling can't keep up with a held-down `j`.
    cursorLine().scrollIntoView({
      block: opts.center ? 'center' : 'nearest',
      behavior: opts.smooth ? 'smooth' : 'auto',
    });
  }
}

const move = (delta) => moveTo(buf().cursor + delta);

/** How many lines fit on screen — the unit for <C-f> and (halved) <C-d>. */
function screenLines() {
  const h = lines()[0] ? lines()[0].getBoundingClientRect().height : 22;
  return Math.max(1, Math.floor(viewport.clientHeight / Math.max(1, h)));
}

/** `zz` / `zt` / `zb`: put the cursor line in the middle / top / bottom. */
function reveal(where) {
  const line = cursorLine();
  if (!line) return;
  const top = line.offsetTop;
  const h = line.offsetHeight;
  const target = where === 'zt' ? top - h
    : where === 'zb' ? top + h - viewport.clientHeight + h
    : top - (viewport.clientHeight - h) / 2;
  viewport.scrollTo({ top: Math.max(0, target), behavior: 'smooth' });
}

/** `{` / `}`: the next blank line that starts a new block of text. */
function paragraph(dir) {
  const ls = lines();
  let i = buf().cursor + dir;
  while (i > 0 && i < ls.length - 1) {
    const blank = !ls[i].textContent.trim();
    const nextFilled = ls[i + dir] && ls[i + dir].textContent.trim();
    if (blank && nextFilled) return i;
    i += dir;
  }
  return dir > 0 ? ls.length - 1 : 0;
}

/* ── buffers / tabline ─────────────────────────────────────────────────── */

function buildTabline() {
  // The spacer, the brand and the GitHub link are authored in the HTML, so
  // they survive with JS off — tabs are inserted ahead of them.
  const before = $('tabline-spacer');
  bufs.forEach((b, i) => {
    const tab = document.createElement('button');
    tab.className = 'tab';
    tab.type = 'button';
    tab.setAttribute('role', 'tab');
    tab.setAttribute('aria-selected', String(i === state.buf));
    tab.setAttribute('aria-controls', b.el.id);
    tab.dataset.idx = String(i);

    const num = document.createElement('span');
    num.className = 'tabnum';
    num.textContent = String(i + 1);
    tab.append(num, document.createTextNode(`${b.icon} ${b.name}`.trim()));

    tab.addEventListener('click', () => { gotoBuf(i); tab.blur(); });
    tabline.insertBefore(tab, before);
  });
}

function syncTabs() {
  Array.from(tabline.querySelectorAll('.tab[data-idx]')).forEach((tab) => {
    const on = Number(tab.dataset.idx) === state.buf;
    tab.setAttribute('aria-selected', String(on));
    if (on) tab.scrollIntoView({ block: 'nearest', inline: 'nearest' });
  });
}

function gotoBuf(idx, opts = {}) {
  if (idx < 0 || idx >= bufs.length) return false;

  // The pattern survives a buffer switch (as in vim); its matches do not.
  clearSearch({ keepPattern: true, quiet: true });
  bufs.forEach((b, i) => b.el.classList.toggle('active', i === idx));
  state.buf = idx;
  if (opts.top) buf().cursor = 0;
  syncTabs();
  render();

  if (opts.top) viewport.scrollTo({ top: 0, behavior: 'auto' });
  else cursorLine().scrollIntoView({ block: 'nearest', behavior: 'auto' });

  // Opaque origins (file://, sandboxed frames) reject replaceState.
  try { history.replaceState(null, '', '#' + buf().key); } catch (_) { /* no deep link */ }
  message(`"${buf().name}" ${buf().lines.length}L`);
  return true;
}

const bufStep = (delta) =>
  gotoBuf((state.buf + delta + bufs.length) % bufs.length);

/** Resolve ":e docs", ":e docs.txt", ":b 4" to a buffer index. */
function findBuf(arg) {
  const want = arg.trim().toLowerCase();
  if (!want) return -1;
  if (/^\d+$/.test(want)) return Number(want) - 1;
  const byKey = bufs.findIndex((b) => b.key === want);
  if (byKey >= 0) return byKey;
  return bufs.findIndex((b) =>
    b.name.toLowerCase() === want
    || b.name.toLowerCase().startsWith(want)
    || b.key.startsWith(want));
}

/* ── search ────────────────────────────────────────────────────────────── */

function clearSearch(opts = {}) {
  state.matches = [];
  state.match = -1;
  if (!opts.keepPattern) state.pattern = '';
  if (HAS_HIGHLIGHT) {
    CSS.highlights.delete('search');
    CSS.highlights.delete('searchcur');
  }
  if (!opts.quiet) message('');
}

function paintMatches() {
  if (!HAS_HIGHLIGHT || !state.hlsearch) return;
  const all = state.matches.map((m) => m.range).filter(Boolean);
  if (!all.length) return;
  const cur = state.matches[state.match];
  CSS.highlights.set('search', new Highlight(...all));
  if (cur && cur.range) CSS.highlights.set('searchcur', new Highlight(cur.range));
}

function search(pattern) {
  clearSearch({ quiet: true });
  if (!pattern) { render(); return; }
  state.pattern = pattern;

  const needle = pattern.toLowerCase();
  const ls = lines();
  const walker = document.createTreeWalker(buf().el, NodeFilter.SHOW_TEXT);
  for (let node = walker.nextNode(); node; node = walker.nextNode()) {
    const hay = node.nodeValue.toLowerCase();
    const line = node.parentElement.closest('.l');
    if (!line) continue;
    const lineIdx = ls.indexOf(line);
    for (let at = hay.indexOf(needle); at !== -1; at = hay.indexOf(needle, at + needle.length)) {
      let range = null;
      if (HAS_HIGHLIGHT) {
        range = document.createRange();
        range.setStart(node, at);
        range.setEnd(node, at + needle.length);
      }
      state.matches.push({ line: lineIdx, range });
    }
  }

  if (!state.matches.length) {
    message(`E486: Pattern not found: ${pattern}`, true);
    render();
    return;
  }
  // Start from the first hit at or after the cursor, the way `/` does.
  const from = state.matches.findIndex((m) => m.line >= buf().cursor);
  state.match = from === -1 ? 0 : from;
  showMatch();
}

function nextMatch(dir) {
  if (!state.pattern) { message('E35: No previous regular expression', true); return; }
  if (!state.matches.length) { search(state.pattern); return; }
  const n = state.matches.length;
  const wrapped = dir > 0 ? state.match + 1 >= n : state.match - 1 < 0;
  state.match = (state.match + dir + n) % n;
  showMatch(wrapped
    ? (dir > 0 ? 'search hit BOTTOM, continuing at TOP'
               : 'search hit TOP, continuing at BOTTOM')
    : null);
}

function showMatch(note) {
  const m = state.matches[state.match];
  if (!m) return;
  paintMatches();
  moveTo(m.line);
  message(note || `/${state.pattern}  [${state.match + 1}/${state.matches.length}]`);
}

/* ── docs ──────────────────────────────────────────────────────────────────
 * The docs buffer is the wiki, rendered into one long help buffer by
 * site/build-docs.mjs. Two ways in: the search box at the top of it, and
 * `:help <topic>` (or the palette), which jumps to a heading.
 */

const docsIdx = bufs.findIndex((b) => b.key === 'docs');
const docsBuf = docsIdx >= 0 ? bufs[docsIdx] : null;

let docLines = null;
let docHeads = null;

/** Every non-blank docs line, tagged with the section and heading above it. */
function docIndex() {
  if (docLines) return docLines;
  docLines = [];
  if (!docsBuf) return docLines;

  let sec = '';
  let head = '';
  docsBuf.lines.forEach((el, idx) => {
    const owner = el.closest('.docsec');
    if (!owner) return;                      // the buffer's own header lines
    const at = owner.dataset.sec || '';
    if (at !== sec) { sec = at; head = ''; }
    const h = el.querySelector('.h2, .h3');
    if (h) head = h.textContent.replace(/^#+\s*/, '').trim();
    const text = el.textContent.replace(/\s+/g, ' ').trim();
    if (text) docLines.push({ idx, text, sec, head, heading: !!h });
  });
  return docLines;
}

/** Anchored headings, for `:help <topic>`. `top` marks a whole section. */
function docTopics() {
  if (docHeads) return docHeads;
  docHeads = [];
  if (!docsBuf) return docHeads;
  docsBuf.el.querySelectorAll('.l[id^="doc-"]').forEach((el) => {
    docHeads.push({
      id: el.id,
      title: el.textContent.replace(/^#+\s*/, '').trim(),
      top: !!el.querySelector('.h1'),
    });
  });
  return docHeads;
}

/** Move to an `id` anywhere in the page, switching buffers if need be. */
function jumpToAnchor(id) {
  const el = document.getElementById(id);
  const owner = el && el.closest('.buf');
  if (!owner) {
    const key = bufs.findIndex((b) => b.key === id);
    return key >= 0 ? gotoBuf(key, { top: true }) : false;
  }

  const bi = bufs.findIndex((b) => b.el === owner);
  if (bi < 0) return false;
  if (bi !== state.buf) gotoBuf(bi);
  const idx = bufs[bi].lines.indexOf(el);
  if (idx >= 0) moveTo(idx, { center: true });
  try { history.replaceState(null, '', `#${id}`); } catch (_) { /* no deep link */ }
  message(el.textContent.replace(/^#+\s*/, '').trim());
  return true;
}

/** The matched run, with ~30 columns of context either side. */
function resultText(row, q) {
  const pre = Math.max(0, row.at - 30);
  const body = row.text.slice(pre, pre + 150);
  const at = row.at - pre;
  const frag = document.createDocumentFragment();
  const hit = document.createElement('span');
  hit.className = 'm';
  hit.textContent = body.slice(at, at + q.length);
  frag.append(
    (pre ? '…' : '') + body.slice(0, at),
    hit,
    body.slice(at + q.length) + (row.text.length > pre + 150 ? '…' : ''),
  );
  return frag;
}

function drawDocResults(q, total) {
  docqResults.replaceChildren();

  if (!q) {
    docqResults.hidden = true;
    docqStatus.textContent = '';
    return;
  }
  if (!state.dhits.length) {
    docqStatus.textContent = 'no matches';
    const empty = document.createElement('div');
    empty.className = 'dempty';
    empty.textContent = `E486: Pattern not found: ${q}`;
    docqResults.append(empty);
    docqResults.hidden = false;
    return;
  }

  docqStatus.textContent = total > state.dhits.length
    ? `${total} matches — first ${state.dhits.length}`
    : `${total} match${total === 1 ? '' : 'es'}`;

  state.dhits.forEach((row, i) => {
    const item = document.createElement('div');
    item.className = 'dres';
    item.setAttribute('role', 'option');
    item.setAttribute('aria-selected', String(i === state.didx));

    const where = document.createElement('span');
    where.className = 'dwhere';
    where.textContent = row.head ? `${row.sec} › ${row.head}` : row.sec;

    const text = document.createElement('span');
    text.className = 'dtext';
    text.append(resultText(row, q));

    item.append(where, text);
    item.addEventListener('mousedown', (e) => { e.preventDefault(); docJump(i); });
    docqResults.append(item);
  });
  docqResults.hidden = false;
}

/** Plain substring search over the docs, headings first. */
function docSearch(query) {
  const q = query.trim().toLowerCase();
  state.dhits = [];
  state.didx = 0;
  if (!q) { drawDocResults('', 0); return; }

  for (const row of docIndex()) {
    const at = row.text.toLowerCase().indexOf(q);
    if (at === -1) continue;
    // Headings answer "where is this documented"; body lines answer "where is
    // it mentioned". The first question is almost always the one being asked.
    const score = (row.heading ? 500 : 0) + (at === 0 ? 40 : 0)
      - Math.min(at, 60) - row.idx / 10000;
    state.dhits.push({ ...row, at, score });
  }

  state.dhits.sort((a, b) => b.score - a.score);
  state.dtotal = state.dhits.length;
  state.dhits = state.dhits.slice(0, 40);
  drawDocResults(query.trim(), state.dtotal);
}

function docMove(delta) {
  if (!state.dhits.length) return;
  state.didx = (state.didx + delta + state.dhits.length) % state.dhits.length;
  drawDocResults(docq.value.trim(), state.dtotal);
  const sel = docqResults.children[state.didx];
  if (sel) sel.scrollIntoView({ block: 'nearest' });
}

function docJump(i) {
  const row = state.dhits[i];
  if (!row) return;
  state.didx = i;
  if (state.buf !== docsIdx) gotoBuf(docsIdx);
  docq.blur();

  moveTo(row.idx, { center: true });
  // Leave the query as the buffer's search pattern, so `n` / `N` walk the
  // rest of the hits the way they would after a `/`. A match that only exists
  // across two spans won't be found by the line search — no error for that.
  search(docq.value.trim());
  if (!state.matches.length) clearSearch({ quiet: true });
  moveTo(row.idx, { center: true });

  const where = row.head ? `${row.sec} › ${row.head}` : row.sec;
  message(`${where}  —  n / N for the next hit, Esc to clear`);
}

function openDocSearch(query) {
  if (!docsBuf) return;
  if (state.buf !== docsIdx) gotoBuf(docsIdx, { top: true });
  else viewport.scrollTo({ top: 0, behavior: 'auto' });
  docq.value = query || '';
  docSearch(docq.value);
  docq.focus();
  message(query ? `docs: ${query}` : 'type to search the docs');
}

/** `:help folds` — a heading if there is one, the search box otherwise. */
function docHelp(topic) {
  if (!docsBuf) { toggleHelp(true); return; }
  const want = topic.trim().toLowerCase();
  if (!want) { gotoBuf(docsIdx, { top: true }); return; }

  const topics = docTopics();
  const eq = (t) => t.title.toLowerCase() === want;
  const starts = (t) => t.title.toLowerCase().startsWith(want);
  const has = (t) => t.title.toLowerCase().includes(want);
  const hit = topics.find((t) => t.top && eq(t)) || topics.find(eq)
    || topics.find((t) => t.top && starts(t)) || topics.find(starts)
    || topics.find(has);

  if (hit) { jumpToAnchor(hit.id); return; }
  openDocSearch(topic.trim());
  message(`no section called "${topic.trim()}" — searching the docs instead`);
}

if (docq) {
  docq.addEventListener('input', () => docSearch(docq.value));
  docq.addEventListener('keydown', (e) => {
    const { key, ctrlKey } = e;
    if (key === 'ArrowDown' || (ctrlKey && key === 'n')) { e.preventDefault(); docMove(1); }
    else if (key === 'ArrowUp' || (ctrlKey && key === 'p')) { e.preventDefault(); docMove(-1); }
    else if (key === 'Enter') { e.preventDefault(); docJump(state.didx); }
    else if (key === 'Escape') {
      e.preventDefault();
      docq.value = '';
      docSearch('');
      docq.blur();
      message('');
    }
  });
}

/* ── commands ──────────────────────────────────────────────────────────── */

function openLink(url) {
  message(`opening ${url}`);
  window.open(url, '_blank', 'noopener');
}

function setOption(arg) {
  const opt = arg.trim().toLowerCase();
  const on = (names) => names.some((n) => opt === n);

  if (on(['nu', 'number'])) state.number = 'abs';
  else if (on(['nonu', 'nonumber'])) state.number = 'off';
  else if (on(['rnu', 'relativenumber'])) state.number = 'rel';
  else if (on(['nornu', 'norelativenumber'])) state.number = state.number === 'rel' ? 'abs' : state.number;
  else if (on(['hls', 'hlsearch'])) { state.hlsearch = true; paintMatches(); }
  else if (on(['nohls', 'nohlsearch'])) { state.hlsearch = false; clearSearch({ keepPattern: true, quiet: true }); }
  else { message(`E518: Unknown option: ${arg}`, true); return; }

  render();
  message(`:set ${opt}`);
}

function toggleHelp(force) {
  const show = force !== undefined ? force : help.hidden;
  if (show && !helpBody.childElementCount) {
    const src = $('site-keymaps');
    if (src) helpBody.innerHTML = src.innerHTML;
  }
  help.hidden = !show;
  state.mode = show ? 'HELP' : 'NORMAL';
  paintStatus();
}

function copyClone() {
  const cmd = `git clone ${REPO}.git`;
  if (navigator.clipboard) {
    navigator.clipboard.writeText(cmd)
      .then(() => message(`yanked "${cmd}" to the clipboard`))
      .catch(() => message(cmd));
  } else {
    message(cmd);
  }
}

/** The palette's roster. `run` is what <CR> fires. */
const COMMANDS = [
  ...bufs.map((b, i) => ({
    name: `:e ${b.name}`,
    desc: `open ${b.name}`,
    run: () => gotoBuf(i, { top: true }),
  })),
  { name: ':bnext', desc: 'next page (gt)', run: () => bufStep(1) },
  { name: ':bprevious', desc: 'previous page (gT)', run: () => bufStep(-1) },
  { name: ':docs', desc: 'search the documentation', run: () => openDocSearch('') },
  ...docTopics().filter((t) => t.top).map((t) => ({
    name: `:help ${t.title}`,
    desc: `docs → ${t.title}`,
    run: () => jumpToAnchor(t.id),
  })),
  ...THEMES.map((t) => ({
    name: `:colorscheme ${t}`,
    desc: `theme → ${THEME_NAMES[t]}`,
    run: () => { setTheme(t); message(`colorscheme ${THEME_NAMES[t]}`); },
  })),
  { name: ':set number', desc: 'absolute line numbers', run: () => setOption('number') },
  { name: ':set relativenumber', desc: 'relative line numbers', run: () => setOption('relativenumber') },
  { name: ':set nonumber', desc: 'hide the gutter', run: () => setOption('nonumber') },
  { name: ':nohlsearch', desc: 'clear search highlighting', run: () => clearSearch() },
  { name: ':keymap', desc: "this page's keys, as an overlay", run: () => toggleHelp(true) },
  { name: ':clone', desc: 'yank the git clone line to the clipboard', run: copyClone },
  { name: ':GitHub', desc: 'open the repository', run: () => openLink(REPO) },
  { name: ':issues', desc: 'open the issue tracker', run: () => openLink(`${REPO}/issues`) },
  { name: ':quit', desc: 'it is a web page — but go on', run: () =>
      message('E37: No write since last star — try :GitHub instead', true) },
];

/** Run a literal cmdline string (what the user typed, arguments and all). */
function execute(raw) {
  const line = raw.trim().replace(/^:+/, '').trim();
  if (!line) return;
  const [head, ...rest] = line.split(/\s+/);
  const arg = rest.join(' ');
  const cmd = head.toLowerCase();

  if (/^\d+$/.test(cmd)) { moveTo(Number(cmd) - 1); return; }

  switch (cmd) {
    case 'e': case 'edit': case 'b': case 'buffer': {
      const idx = findBuf(arg);
      if (idx >= 0 && gotoBuf(idx, { top: true })) return;
      message(`E94: No matching buffer for ${arg}`, true);
      return;
    }
    case 'bn': case 'bnext': case 'tabn': case 'tabnext': bufStep(1); return;
    case 'bp': case 'bprev': case 'bprevious': case 'tabp': case 'tabprevious': bufStep(-1); return;
    case 'colo': case 'colorscheme': {
      if (!arg) { message(`colorscheme ${THEME_NAMES[document.documentElement.dataset.theme]}`); return; }
      const theme = resolveTheme(arg);
      if (theme) { setTheme(theme); message(`colorscheme ${THEME_NAMES[theme]}`); }
      else message(`E185: Cannot find color scheme "${arg}"`, true);
      return;
    }
    case 'se': case 'set': setOption(arg); return;
    case 'noh': case 'nohl': case 'nohls': case 'nohlsearch': clearSearch(); return;
    case 'h': case 'help': if (arg) docHelp(arg); else toggleHelp(true); return;
    case 'keymap': case 'keys': toggleHelp(true); return;
    case 'doc': case 'docs': openDocSearch(arg); return;
    case 'clone': copyClone(); return;
    case 'github': case 'gh': openLink(REPO); return;
    case 'issues': openLink(`${REPO}/issues`); return;
    case 'q': case 'q!': case 'qa': case 'qa!': case 'quit': case 'wq':
      message('E37: No write since last star — try :GitHub instead', true);
      return;
    default:
      message(`E492: Not an editor command: ${line}`, true);
  }
}

/* ── palette ───────────────────────────────────────────────────────────── */

/** Subsequence match. Returns null on a miss, or {score, hit:[indices]}. */
function fuzzy(needle, haystack) {
  if (!needle) return { score: 0, hit: [] };
  const hay = haystack.toLowerCase();
  const hit = [];
  let score = 0;
  let at = 0;
  let streak = 0;
  for (const ch of needle.toLowerCase()) {
    if (ch === ' ') continue;
    const found = hay.indexOf(ch, at);
    if (found === -1) return null;
    streak = found === at ? streak + 1 : 0;
    score += 12 + streak * 6 - Math.min(found - at, 12);
    if (found === 0 || /[\s:_\-.]/.test(hay[found - 1])) score += 10;
    hit.push(found);
    at = found + 1;
  }
  return { score, hit };
}

function filterPalette(query) {
  const q = query.replace(/^:/, '');
  state.pitems = COMMANDS
    .map((cmd) => {
      const m = fuzzy(q, cmd.name);
      return m ? { cmd, ...m } : null;
    })
    .filter(Boolean)
    .sort((a, b) => b.score - a.score);
  state.pidx = 0;
  drawPalette();
}

function drawPalette() {
  palette.replaceChildren();
  if (!state.pitems.length) {
    const empty = document.createElement('div');
    empty.className = 'pempty';
    empty.textContent = 'no matching command — <CR> runs what you typed';
    palette.appendChild(empty);
    return;
  }

  state.pitems.forEach((item, i) => {
    const row = document.createElement('div');
    row.className = 'pitem';
    row.setAttribute('role', 'option');
    row.setAttribute('aria-selected', String(i === state.pidx));

    const name = document.createElement('span');
    name.className = 'pname';
    const hits = new Set(item.hit);
    Array.from(item.cmd.name).forEach((ch, at) => {
      if (hits.has(at)) {
        const m = document.createElement('span');
        m.className = 'm';
        m.textContent = ch;
        name.appendChild(m);
      } else {
        name.appendChild(document.createTextNode(ch));
      }
    });

    const desc = document.createElement('span');
    desc.className = 'pdesc';
    desc.textContent = item.cmd.desc;

    row.append(name, desc);
    row.addEventListener('mousemove', () => {
      if (state.pidx === i) return;
      state.pidx = i;
      drawPalette();
    });
    row.addEventListener('mousedown', (e) => {
      e.preventDefault();
      const run = item.cmd.run;
      closeCmdline();
      run();
    });
    palette.appendChild(row);
  });

  const sel = palette.children[state.pidx];
  if (sel) sel.scrollIntoView({ block: 'nearest' });
}

function movePalette(delta) {
  if (!state.pitems.length) return;
  state.pidx = (state.pidx + delta + state.pitems.length) % state.pitems.length;
  drawPalette();
}

/* ── cmdline ───────────────────────────────────────────────────────────── */

let chWidth = 0;

function measureCh() {
  const probe = document.createElement('span');
  probe.style.cssText = 'position:absolute;visibility:hidden;white-space:pre';
  probe.textContent = '0'.repeat(40);
  cmdline.appendChild(probe);
  chWidth = probe.getBoundingClientRect().width / 40;
  probe.remove();
}

function moveCaret() {
  if (!chWidth) measureCh();
  const col = cmdinput.selectionStart ?? cmdinput.value.length;
  const left = cmdinput.offsetLeft + col * chWidth - cmdinput.scrollLeft;
  blockcaret.style.left = `${left}px`;
}

function openCmdline(prefix) {
  state.mode = prefix === ':' ? 'COMMAND' : 'SEARCH';
  state.pending = '';
  cmdprompt.textContent = prefix;
  cmdinput.value = '';
  cmdline.hidden = false;
  blockcaret.hidden = false;
  message('');
  cmdinput.focus();

  if (prefix === ':') {
    filterPalette('');
    palette.hidden = false;
  }
  paintStatus();
  moveCaret();
}

function closeCmdline() {
  state.mode = 'NORMAL';
  cmdline.hidden = true;
  palette.hidden = true;
  cmdinput.blur();
  cmdinput.value = '';
  paintStatus();
}

cmdinput.addEventListener('input', () => {
  if (state.mode === 'COMMAND') filterPalette(cmdinput.value);
  moveCaret();
});
cmdinput.addEventListener('click', moveCaret);
cmdinput.addEventListener('keyup', moveCaret);

cmdinput.addEventListener('keydown', (e) => {
  const { key, ctrlKey } = e;

  if (key === 'Escape' || (ctrlKey && key === 'c')) {
    e.preventDefault();
    closeCmdline();
    message('');
    return;
  }

  if (state.mode === 'COMMAND') {
    if (key === 'ArrowDown' || (ctrlKey && key === 'n')) { e.preventDefault(); movePalette(1); return; }
    if (key === 'ArrowUp' || (ctrlKey && key === 'p')) { e.preventDefault(); movePalette(-1); return; }
    if (key === 'Tab') {
      // Complete the selection into the line rather than running it.
      e.preventDefault();
      const pick = state.pitems[state.pidx];
      if (pick) {
        cmdinput.value = pick.cmd.name.replace(/^:/, '');
        filterPalette(cmdinput.value);
        moveCaret();
      }
      return;
    }
  }

  if (key === 'Enter') {
    e.preventDefault();
    const typed = cmdinput.value.trim();
    const mode = state.mode;
    const pick = state.pitems[state.pidx];
    closeCmdline();

    if (mode === 'SEARCH') { search(typed); return; }
    // A bare fuzzy query runs the highlighted command; anything with an
    // argument of its own is executed literally.
    if (pick && !/\s/.test(typed)) pick.cmd.run();
    else execute(typed);
  }
});

/* ── normal mode ───────────────────────────────────────────────────────── */

function followLink() {
  const link = cursorLine() && cursorLine().querySelector('a[href]');
  if (!link) { message('E447: No link on this line', true); return; }
  const href = link.getAttribute('href');
  if (href.startsWith('#')) { jumpToAnchor(href.slice(1)); return; }
  openLink(link.href);
}

document.addEventListener('keydown', (e) => {
  if (state.mode === 'COMMAND' || state.mode === 'SEARCH') return;
  if (e.target instanceof HTMLInputElement || e.target.isContentEditable) return;
  if (e.metaKey || e.altKey) return;

  const { key, ctrlKey } = e;

  if (state.mode === 'HELP') {
    if (key === 'Escape' || key === '?' || key === 'q') { e.preventDefault(); toggleHelp(false); }
    return;
  }

  if (ctrlKey) {
    const page = screenLines();
    const half = Math.max(1, Math.floor(page / 2));
    // <C-Tab> walks the tabline, as the statusline hint advertises.
    if (key === 'Tab') { e.preventDefault(); bufStep(e.shiftKey ? -1 : 1); }
    else if (key === 'd') { e.preventDefault(); move(half); }
    else if (key === 'u') { e.preventDefault(); move(-half); }
    else if (key === 'f') { e.preventDefault(); move(page); }
    else if (key === 'b') { e.preventDefault(); move(-page); }
    return;
  }

  // `g` and `z` chords.
  if (state.pending) {
    e.preventDefault();
    const chord = state.pending + key;
    state.pending = '';
    switch (chord) {
      case 'gg': moveTo(0); break;
      case 'gt': bufStep(1); break;
      case 'gT': bufStep(-1); break;
      case 'gx': followLink(); break;
      case 'zz': case 'zt': case 'zb': reveal(chord); break;
      default: paintStatus(); return;
    }
    paintStatus();
    return;
  }

  switch (key) {
    case 'j': case 'ArrowDown': e.preventDefault(); move(1); break;
    case 'k': case 'ArrowUp': e.preventDefault(); move(-1); break;
    case 'g': case 'z': e.preventDefault(); state.pending = key; paintStatus(); return;
    case 'G': e.preventDefault(); moveTo(lines().length - 1); break;
    case '}': e.preventDefault(); moveTo(paragraph(1)); break;
    case '{': e.preventDefault(); moveTo(paragraph(-1)); break;
    case ':': e.preventDefault(); openCmdline(':'); return;
    case '/': e.preventDefault(); openCmdline('/'); return;
    case 'n': e.preventDefault(); nextMatch(1); break;
    case 'N': e.preventDefault(); nextMatch(-1); break;
    case '?': e.preventDefault(); toggleHelp(); return;
    case 'Enter': e.preventDefault(); followLink(); break;
    case 'Escape': e.preventDefault(); clearSearch(); render(); break;
    default:
      if (/^[1-9]$/.test(key) && Number(key) <= bufs.length) {
        e.preventDefault();
        gotoBuf(Number(key) - 1, { top: true });
      }
      return;
  }
  paintStatus();
});

/* ── mouse & misc ──────────────────────────────────────────────────────── */

// Cross-references stay inside the page: they move the cursor rather than
// navigating, so the docs read as one buffer.
document.addEventListener('click', (e) => {
  const link = e.target.closest('a[href^="#"]');
  if (!link) return;
  e.preventDefault();
  jumpToAnchor(link.getAttribute('href').slice(1));
});

viewport.addEventListener('click', (e) => {
  if (e.target.closest('a')) return;
  const line = e.target.closest('.l');
  if (!line) return;
  const idx = lines().indexOf(line);
  if (idx >= 0) moveTo(idx, { scroll: false });
});

viewport.addEventListener('scroll', paintStatus, { passive: true });

help.addEventListener('click', (e) => { if (e.target === help) toggleHelp(false); });
fab.addEventListener('click', () => openCmdline(':'));
window.addEventListener('resize', () => { chWidth = 0; });

/* ── boot ──────────────────────────────────────────────────────────────── */

(function init() {
  let saved = null;
  try { saved = localStorage.getItem('ctrlvim.theme'); } catch (_) { /* ignore */ }
  setTheme(THEMES.includes(saved) ? saved : 'tokyonight');

  buildTabline();

  const hash = location.hash.replace(/^#/, '');
  const wanted = bufs.findIndex((b) => b.key === hash.replace(/^buf-/, ''));
  if (wanted > 0) {
    bufs.forEach((b, i) => b.el.classList.toggle('active', i === wanted));
    state.buf = wanted;
    syncTabs();
  }

  render();

  // A deep link into the docs (…/#doc-folds) lands on the line itself.
  if (wanted < 0 && hash) jumpToAnchor(hash);
  const touch = typeof matchMedia === 'function' && matchMedia('(hover: none)').matches;
  message(touch
    ? `"${buf().name}" ${buf().lines.length}L — tap a tab to switch files`
    : `"${buf().name}" ${buf().lines.length}L — press : for commands, ? for keys`);
})();
