#!/usr/bin/env node
/* Generates the site's `docs.txt` buffer from the ctrlvim wiki.
 *
 *   node site/build-docs.mjs [path-to-wiki-checkout]
 *
 * The wiki (github.com/CtrlUserKnown/ctrlvim.wiki) is a separate repository,
 * so its markdown is rendered here and the result is committed into
 * site/index.html between the DOCS markers. Pages deploy is a plain artifact
 * upload — nothing runs at build time — so the docs have to already be HTML.
 *
 * The output is the same line-per-`<div class="l">` markup the hand-written
 * buffers use: the page is an editor, and the docs are a buffer in it. No new
 * layout, no new widgets — headings, box-drawn tables, code blocks and links,
 * all made of text.
 */

import { readFileSync, writeFileSync, existsSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { homedir } from 'node:os';
import { join, dirname } from 'node:path';

const HERE = dirname(fileURLToPath(import.meta.url));
const INDEX = join(HERE, 'index.html');
const WIKI = process.argv[2]
  || process.env.CTRLVIM_WIKI
  || join(homedir(), 'development', 'wikis', 'ctrlvim.wiki');

const BEGIN = '<!-- BEGIN GENERATED DOCS -->';
const END = '<!-- END GENERATED DOCS -->';

/** Pages that exist for the wiki's own navigation, not for the site. */
const SKIP = new Set(['Home', '_Sidebar', '_Footer']);

/** One-line summaries for the contents list, keyed by page. */
const BLURBS = {
  Installation: 'requirements, the installer, the Makefile, where files land',
  'Getting-Started': 'launching, the dashboard, opening files, tabs, quitting',
  Keybindings: 'every default mapping, by mode and by panel',
  Editing: 'motions, operators, text objects, macros, marks, folds, tags',
  'Ex-Commands': 'the `:` command set, grouped by what it acts on',
  'Search-and-Replace': 'patterns, `:s`, `:global`, project-wide replace, quickfix',
  Configuration: 'every table in `config.toml`, with examples',
  Options: 'the `:set` options, their defaults and their scope',
  Themes: 'switching, the roster, what a theme defines',
  'Tools-and-Linting': 'the tool registry, installing them, `:Lint`',
  'AI-Suggestions': 'building it in, switching it on, keys, cost',
  'Plugins-and-Scripting': 'the Lua API, Vimscript, jobs, RPC, writing a plugin',
  Architecture: 'the crate map and the invariants behind the code',
};

const RULE = '─'.repeat(74);   // a rule inside a page (markdown's `---`)
const SEP = '═'.repeat(74);    // the join between two pages

/* ── text helpers ──────────────────────────────────────────────────────── */

const esc = (s) => s.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;');
const width = (s) => Array.from(s).length;
const pad = (s, n) => ' '.repeat(Math.max(0, n - width(s)));

/** GitHub's heading-anchor rules, near enough for the anchors we link to. */
const slug = (s) => s
  .toLowerCase()
  .replace(/[`*_[\]()]/g, '')
  .replace(/[^a-z0-9]+/g, '-')
  .replace(/^-+|-+$/g, '');

/** Markdown with its inline markup removed — for measuring table columns. */
const plain = (md) => md
  .replace(/`([^`]*)`/g, '$1')
  .replace(/\[([^\]]*)\]\([^)]*\)/g, '$1')
  .replace(/<((?:https?|mailto):[^>]*)>/g, '$1')
  .replace(/\*\*([^*]+)\*\*/g, '$1')
  .replace(/\*([^*]+)\*/g, '$1')
  .replace(/\\([|*_`])/g, '$1');

/* ── link resolution ───────────────────────────────────────────────────── */

/** Every `id` the generated docs will carry, so wiki links can be checked. */
const anchors = new Set();

function docHref(target) {
  if (/^(https?:|mailto:|#)/.test(target)) return null;
  const [page, frag] = target.split('#');
  const base = `doc-${slug(page)}`;
  if (frag) {
    const deep = `${base}-${slug(frag)}`;
    if (anchors.has(deep)) return deep;
  }
  return anchors.has(base) ? base : 'doc-contents';
}

/* ── inline markup ─────────────────────────────────────────────────────── */

const INLINE = new RegExp([
  // A code span is fenced by as many backticks as it opened with, so that
  // ``  `{mark}` `` — a literal backtick in the docs — survives.
  '(?<fence>`+)(?<code>[^]*?)\\k<fence>',
  // Labels nest one level of brackets: [`[ui] icons`](Configuration#ui).
  '\\[(?<label>(?:[^\\][\\\\]|\\\\.|\\[[^\\]]*\\])*)\\]\\((?<target>[^)]*)\\)',
  '<(?<auto>(?:https?|mailto):[^>]+)>',
  '\\*\\*(?<bold>[^*]+)\\*\\*',
  '\\*(?<italic>[^\\s*][^*]*)\\*',
  // A `**` with no partner on this line opens (or closes) one that the source
  // wrapped across two lines — the buffer keeps the source's line breaks.
  '(?<dangle>\\*\\*)',
].map((p) => `(?:${p})`).join('|'), 'g');

/** CommonMark strips one padding space from ` x `, so ` `` ` reads right. */
const unpad = (code) => (/^ .* $/.test(code) && code.trim() ? code.slice(1, -1) : code);

/**
 * Render one source line. `open` carries an unterminated `**` into the next
 * line of the same paragraph; the return value says whether it is still open.
 */
function inline(md, open = false) {
  let out = open ? '<span class="b">' : '';
  let bold = open;
  let at = 0;

  for (const m of md.matchAll(INLINE)) {
    out += esc(md.slice(at, m.index));
    at = m.index + m[0].length;
    const g = m.groups;

    if (g.code !== undefined) {
      out += `<span class="code">${esc(unpad(g.code))}</span>`;
    } else if (g.label !== undefined) {
      const anchor = docHref(g.target);
      const body = inline(g.label.replace(/\\([[\]])/g, '$1')).html;
      out += anchor
        ? `<a class="xref" href="#${anchor}">${body}</a>`
        : `<a href="${esc(g.target)}">${body}</a>`;
    } else if (g.auto !== undefined) {
      out += `<a href="${esc(g.auto)}">${esc(g.auto)}</a>`;
    } else if (g.bold !== undefined) {
      out += `<span class="b">${inline(g.bold).html}</span>`;
    } else if (g.italic !== undefined) {
      out += `<span class="i">${inline(g.italic).html}</span>`;
    } else {
      out += bold ? '</span>' : '<span class="b">';
      bold = !bold;
    }
  }

  out += esc(md.slice(at));
  return { html: bold ? `${out}</span>` : out, open: bold };
}

/** Most callers render a line at a time and don't care about continuations. */
const one = (md) => inline(md).html;

/* ── block parsing ─────────────────────────────────────────────────────── */

/** A page becomes a flat list of blocks; blocks become lines of HTML. */
function blocks(md) {
  const src = md.replace(/\r\n/g, '\n').split('\n');
  const out = [];
  let i = 0;

  while (i < src.length) {
    const line = src[i];

    // Fences inside a list item are indented; the body is dedented with them.
    const fence = /^(\s*)```(.*)$/.exec(line);
    if (fence) {
      const [, indent, lang] = fence;
      const body = [];
      for (i += 1; i < src.length && !/^\s*```/.test(src[i]); i += 1) {
        body.push(src[i].startsWith(indent) ? src[i].slice(indent.length) : src[i]);
      }
      i += 1;
      out.push({ kind: 'code', lang: lang.trim(), body });
      continue;
    }

    if (/^#{1,4} /.test(line)) {                                // heading
      const level = line.match(/^#+/)[0].length;
      out.push({ kind: 'head', level, text: line.replace(/^#+ /, '').trim() });
      i += 1;
      continue;
    }

    if (/^\s*(-{3,}|\*{3,}|_{3,})\s*$/.test(line)) {            // horizontal rule
      out.push({ kind: 'rule' });
      i += 1;
      continue;
    }

    if (/^\s*\|/.test(line) && /^\s*\|[\s:|-]+\|?\s*$/.test(src[i + 1] || '')) {
      const rows = [];                                          // table
      const head = row(line);
      i += 2;
      while (i < src.length && /^\s*\|/.test(src[i])) { rows.push(row(src[i])); i += 1; }
      out.push({ kind: 'table', head, rows });
      continue;
    }

    if (/^\s*>/.test(line)) {                                   // blockquote
      const body = [];
      while (i < src.length && /^\s*>/.test(src[i])) {
        body.push(src[i].replace(/^\s*>\s?/, ''));
        i += 1;
      }
      out.push({ kind: 'quote', body });
      continue;
    }

    if (!line.trim()) { out.push({ kind: 'blank' }); i += 1; continue; }

    out.push({ kind: 'text', text: line });                     // prose / lists
    i += 1;
  }
  return out;
}

const row = (line) => line
  .trim()
  .replace(/^\|/, '')
  .replace(/\|\s*$/, '')
  .split(/(?<!\\)\|/)
  .map((c) => c.trim());

/* ── rendering ─────────────────────────────────────────────────────────── */

const L = (html, attrs = '') => `<div class="l"${attrs}>${html}</div>`;
const dim = (s) => `<span class="dim">${esc(s)}</span>`;

/** Comments carry the whole point of most of these snippets — keep them dim. */
function codeLine(text, lang) {
  const mark = lang === 'vim' ? '"' : lang === 'lua' ? '--' : '#';
  const whole = new RegExp(`^(\\s*)(${mark === '--' ? '--' : `[${mark}]`}.*)$`);
  const trail = new RegExp(`^(.*?)(\\s{2,}${mark === '--' ? '--' : `[${mark}]`}.*)$`);

  let m = whole.exec(text);
  if (m) return `${esc(m[1])}<span class="c">${esc(m[2])}</span>`;
  m = trail.exec(text);
  if (m) return `${esc(m[1])}<span class="c">${esc(m[2])}</span>`;
  return esc(text);
}

function renderTable(t) {
  const cols = t.head.length;
  const cells = [t.head, ...t.rows].map((r) => {
    const padded = Array.from({ length: cols }, (_, c) => r[c] ?? '');
    return padded.map((md) => ({ html: one(md), len: width(plain(md)) }));
  });
  const w = Array.from({ length: cols }, (_, c) =>
    Math.max(...cells.map((r) => r[c].len)));

  const bar = (l, mid, r) => dim(l + w.map((n) => '─'.repeat(n + 2)).join(mid) + r);
  const line = (cs, cls) => {
    const body = cs.map((cell, c) =>
      ` ${cls ? `<span class="${cls}">${cell.html}</span>` : cell.html}${' '.repeat(w[c] - cell.len)} `);
    return dim('│') + body.join(dim('│')) + dim('│');
  };

  const [head, ...rows] = cells;
  return [
    '<div class="box">',
    L(bar('┌', '┬', '┐')),
    L(line(head, 'th')),
    L(bar('├', '┼', '┤')),
    ...rows.map((r) => L(line(r))),
    L(bar('└', '┴', '┘')),
    '</div>',
  ];
}

/** `- item` / `1. item`, keeping the source's indentation. */
function renderText(text, open) {
  const bullet = /^(\s*)[-*+]\s+(.*)$/.exec(text);
  if (bullet) {
    const r = inline(bullet[2], open);
    return { html: L(`${bullet[1]}${dim('•')} ${r.html}`), open: r.open };
  }
  const num = /^(\s*)(\d+)\.\s+(.*)$/.exec(text);
  if (num) {
    const r = inline(num[3], open);
    return { html: L(`${num[1]}<span class="n">${num[2]}.</span> ${r.html}`), open: r.open };
  }
  const r = inline(text, open);
  return { html: L(r.html), open: r.open };
}

function renderPage(page, md) {
  const bs = blocks(md);
  const title = bs.find((b) => b.kind === 'head' && b.level === 1);
  const base = `doc-${slug(page)}`;
  const subs = bs.filter((b) => b.kind === 'head' && b.level === 2);

  const out = [
    `<div class="docsec" data-sec="${esc(title ? title.text : page)}">`,
    L(''),
    L(dim(SEP)),
    L(`<span class="h1">${esc(title ? title.text : page)}</span>`, ` id="${base}"`),
  ];

  if (subs.length) {
    const links = subs
      .map((s) => `<a class="xref" href="#${base}-${slug(s.text)}">${one(s.text)}</a>`)
      .join(dim(' · '));
    out.push(L(`${dim('on this page:')} ${links}`));
  }

  let open = false;   // an unterminated `**`, carried down a wrapped paragraph
  for (const b of bs) {
    if (b.kind !== 'text') open = false;

    switch (b.kind) {
      case 'head': {
        if (b.level === 1) break;                 // already emitted as the title
        const id = ` id="${base}-${slug(b.text)}"`;
        const cls = b.level === 2 ? 'h2' : 'h3';
        const marks = '#'.repeat(b.level);
        if (out[out.length - 1] !== L('')) out.push(L(''));
        out.push(L(`<span class="${cls}">${marks} ${one(b.text)}</span>`, id));
        break;
      }
      case 'text': {
        const r = renderText(b.text, open);
        out.push(r.html);
        open = r.open;
        break;
      }
      case 'blank': out.push(L('')); break;
      case 'rule': out.push(L(dim(RULE))); break;
      case 'table': out.push(...renderTable(b)); break;
      case 'code':
        out.push(
          '<div class="cb">',
          ...b.body.map((c) => L(`${dim('▏')}  ${codeLine(c, b.lang)}`)),
          '</div>',
        );
        break;
      case 'quote': {
        let qopen = false;
        out.push('<div class="note">');
        for (const q of b.body) {
          if (!q.trim()) { out.push(L(dim('┃'))); qopen = false; continue; }
          const r = inline(q, qopen);
          out.push(L(`${dim('┃')}  ${r.html}`));
          qopen = r.open;
        }
        out.push('</div>');
        break;
      }
      default: break;
    }
  }

  out.push('</div>');
  return out;
}

/* ── contents ──────────────────────────────────────────────────────────── */

function renderContents(groups) {
  const nameWidth = Math.max(...groups.flatMap((g) => g.pages.map((p) => width(title(p)))));
  const out = [
    '<div class="docsec" data-sec="Contents">',
    L(`<span class="h1">Contents</span>`, ' id="doc-contents"'),
    L(''),
  ];
  for (const g of groups) {
    out.push(L(`<span class="h2">${esc(g.title)}</span>`), L(''));
    for (const p of g.pages) {
      const label = title(p);
      out.push(L(
        `  <a class="xref" href="#doc-${slug(p)}">${esc(label)}</a>${pad(label, nameWidth)}   `
        + `<span class="dim">${one(BLURBS[p] || '')}</span>`,
      ));
    }
    out.push(L(''));
  }
  out.push('</div>');
  return out;
}

const title = (page) => page.replace(/-/g, ' ');

/* ── sidebar → the page order and grouping ─────────────────────────────── */

function readGroups() {
  const sidebar = join(WIKI, '_Sidebar.md');
  if (!existsSync(sidebar)) throw new Error(`no _Sidebar.md in ${WIKI}`);
  const groups = [];
  for (const line of readFileSync(sidebar, 'utf8').split('\n')) {
    const heading = /^\*\*(.+?)\*\*\s*$/.exec(line);
    if (heading) { groups.push({ title: heading[1], pages: [] }); continue; }
    const link = /^\s*[-*]\s*\[[^\]]*\]\(([^)]+)\)/.exec(line);
    if (link && groups.length) {
      const page = link[1].split('#')[0];
      if (!SKIP.has(page)) groups[groups.length - 1].pages.push(page);
    }
  }
  return groups.filter((g) => g.pages.length);
}

/* ── main ──────────────────────────────────────────────────────────────── */

function main() {
  const groups = readGroups();
  const pages = groups.flatMap((g) => g.pages);

  // Two passes: anchors first, so a cross-page link can be resolved (and a
  // dead one caught) while the page that contains it is being rendered.
  anchors.add('doc-contents');
  const sources = new Map();
  for (const page of pages) {
    const file = join(WIKI, `${page}.md`);
    if (!existsSync(file)) throw new Error(`missing wiki page: ${file}`);
    const md = readFileSync(file, 'utf8');
    sources.set(page, md);
    anchors.add(`doc-${slug(page)}`);
    for (const line of md.split('\n')) {
      const h = /^(#{2,4}) (.+)$/.exec(line);
      if (h) anchors.add(`doc-${slug(page)}-${slug(h[2].trim())}`);
    }
  }

  const lines = [
    ...renderContents(groups),
    ...pages.flatMap((page) => renderPage(page, sources.get(page))),
  ];

  const html = readFileSync(INDEX, 'utf8');
  const from = html.indexOf(BEGIN);
  const to = html.indexOf(END);
  if (from === -1 || to === -1) throw new Error('DOCS markers not found in index.html');

  const indent = '      ';
  const body = lines.map((l) => indent + l).join('\n');
  const next = `${html.slice(0, from + BEGIN.length)}\n${body}\n${indent}${html.slice(to)}`;
  writeFileSync(INDEX, next);

  const count = lines.filter((l) => l.startsWith('<div class="l"')).length;
  process.stdout.write(
    `docs: ${pages.length} pages, ${count} lines, ${(next.length / 1024).toFixed(0)}K index.html\n`,
  );
}

main();
