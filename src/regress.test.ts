// Source-only checks: no app imports, credentials, data directories or network.
import { readFileSync, readdirSync, existsSync, statSync } from 'node:fs';
import { resolve, relative } from 'node:path';
import { fileURLToPath } from 'node:url';
import ts from 'typescript';
import { describe, expect, it } from 'vitest';

const root = fileURLToPath(new URL('..', import.meta.url));
const read = (file: string) => readFileSync(resolve(root, file), 'utf8');
const files = (dir: string, extension: string, recursive = false): string[] =>
  readdirSync(resolve(root, dir), { withFileTypes: true }).flatMap(e => e.isDirectory()
    ? recursive ? files(`${dir}/${e.name}`, extension, true) : []
    : e.name.endsWith(extension) && !e.name.endsWith('.test.ts') ? [`${dir}/${e.name}`] : []);
const front = files('src', '.ts'), pages = files('site', '.html');
type Lang = 'nl' | 'en';
type Text = { file: string; offset: number; text: string; lang?: Lang };
const at = (t: Text) => `${t.file}:${read(t.file).slice(0, t.offset).split('\n').length}`;
const decode = (s: string) => s.replace(/&(?:amp|gt|lt|quot|apos|#39|nbsp);/g, e =>
  ({ '&amp;': '&', '&gt;': '>', '&lt;': '<', '&quot;': '"', '&apos;': "'", '&#39;': "'", '&nbsp;': ' ' })[e]!)
  .replace(/&#(x[0-9a-f]+|\d+);/gi, (_, n) => String.fromCodePoint(n[0] === 'x' ? parseInt(n.slice(1), 16) : Number(n)));

function source(file: string) { return ts.createSourceFile(file, read(file), ts.ScriptTarget.Latest, true); }
function visit(node: ts.Node, fn: (n: ts.Node) => void) { fn(node); node.forEachChild(n => visit(n, fn)); }
function literal(n: ts.Node | undefined): string | undefined {
  return n && (ts.isStringLiteral(n) || ts.isNoSubstitutionTemplateLiteral(n)) ? n.text : undefined;
}
function bilingual(file: string): Text[] {
  const result: Text[] = [];
  visit(source(file), n => {
    if (ts.isCallExpression(n) && n.expression.getText() === 'L') {
      n.arguments.slice(0, 2).forEach((a, i) => {
        const text = literal(a);
        if (text !== undefined) result.push({ file, offset: a.getStart(), text, lang: i ? 'en' : 'nl' });
      });
    }
  });
  return result;
}

// Keep inline markup together (including <b>A</b> → <b>B</b>), and inherit lang.
function htmlTexts(file: string): Text[] {
  const result: Text[] = [], stack: { tag: string; lang?: Lang }[] = [];
  let buffer = '', offset = 0, lang: Lang | undefined;
  const flush = () => { if (buffer.trim()) result.push({ file, offset, text: decode(buffer), lang }); buffer = ''; };
  for (const m of read(file).matchAll(/<!--[\s\S]*?-->|<[^>]+>|[^<]+/g)) {
    const token = m[0];
    if (!token.startsWith('<')) { if (!buffer) offset = m.index; buffer += token; continue; }
    const tag = token.match(/^<\/?([\w-]+)/)?.[1];
    if (!tag) continue;
    const block = /^(p|div|section|article|td|tr|h[1-6]|li|br)$/.test(tag);
    const nextLang = token.match(/\blang=["'](nl|en)["']/)?.[1] as Lang | undefined;
    if (block || nextLang) flush();
    if (/^(b|strong)$/.test(tag)) { if (!buffer) offset = m.index; buffer += token; }
    if (token.startsWith('</')) {
      const i = stack.map(s => s.tag).lastIndexOf(tag);
      if (i >= 0) { if (stack[i].lang !== lang) flush(); stack.splice(i); }
    } else if (!/^(meta|link|img|br|input|hr)$/.test(tag)) stack.push({ tag, lang: nextLang ?? lang });
    lang = stack.at(-1)?.lang;
  }
  flush(); return result;
}

function labels() {
  const result = { nl: new Set<string>(), en: new Set<string>() };
  for (const file of ['src/main.ts', 'src/wizard.ts']) {
    const ast = source(file), raw = read(file);
    visit(ast, n => {
      if (!ts.isCallExpression(n) || n.expression.getText() !== 'L') return;
      const parent = n.parent;
      const uiCall = ts.isCallExpression(parent) && /^(field|toggle|frame|dialogHead|actRow)$/.test(parent.expression.getText());
      const before = raw.slice(Math.max(0, n.getStart() - 200), n.getStart());
      const markup = /<(?:h[1-6]|button|summary|dt)\b[^>]*>[^<]*\$\{$/.test(before);
      if (!uiCall && !markup) return;
      n.arguments.slice(0, 2).forEach((a, i) => { const s = literal(a); if (s && !s.includes('→')) result[i ? 'en' : 'nl'].add(s); });
    });
    if (file.endsWith('main.ts')) {
      const tabs = raw.match(/const TABS:[^=]+= ([^;]+);/)![1];
      for (const m of tabs.matchAll(/\['[^']+', '([^']+)', '([^']+)'\]/g)) { result.nl.add(m[1]); result.en.add(m[2]); }
      // Literal, language-independent labels in the sidebar/account panel.
      for (const m of raw.matchAll(/(?:field|actRow)\('([^']+)'/g)) { result.nl.add(m[1]); result.en.add(m[1]); }
    }
  }
  return result;
}
const ui = labels();
let checkedPaths = 0;
// These are outside OV Refund: Windows notification settings / SmartScreen,
// and NS's Dutch-only refund history. Exact paths only, never individual labels.
const outside = [
  'Instellingen → Systeem → Meldingen → OV Refund', 'Settings → System → Notifications → OV Refund',
  'Meer informatie → Toch uitvoeren', 'More info → Run anyway', 'Mijn NS → Mijn teruggaven',
  // Illustrative train routes in the website mock table, not navigation.
  'Utrecht C. → Zwolle', 'Leiden C. → Den Haag C.', 'Amersfoort C. → Breda', 'Arnhem C. → Utrecht C.',
];

function navigation(t: Text): string[] {
  // A bold run is one path ("<b>A</b> → <b>B</b>" included); prose around it is separate.
  let text = decode(t.text).replace(/<\/(b|strong)>\s*→\s*<\1>/g, ' → ')
    .replace(/<\/?(?:b|strong)>/g, '\n').replace(/<[^>]*>/g, '').replace(/[*`]/g, '');
  for (const path of outside) text = text.replaceAll(path, '');
  const errors: string[] = [];
  // Sentence/markup boundaries separate paths from prose; a trailing arrow is an icon.
  for (const part of text.split(/[.;\n|()]/)) {
    if (!part.includes('→') || !part.split('→').at(-1)?.trim()) continue;
    const segments = part.split('→').map(s => s.trim());
    checkedPaths++;
    const lang = t.lang ?? (segments[0].includes('Settings') || segments[0].includes('Notifications') ? 'en' : 'nl');
    const known = [...ui[lang]].sort((a, b) => b.length - a.length);
    segments.forEach((s, i) => {
      const found = known.find(label => i === 0 ? s === label || s.endsWith(` ${label}`) || s.endsWith(`: ${label}`)
        : i === segments.length - 1 ? s === label || s.startsWith(`${label} `) : s === label);
      if (!found) errors.push(`${at(t)} [${lang}]: bad navigation segment "${s}"`);
    });
  }
  return errors;
}

function checkSiteTarget(url: URL): string | undefined {
  const prefix = '/OV-Refund/';
  if (!url.pathname.startsWith(prefix)) return `outside website root: ${url.href}`;
  let file = resolve(root, 'site', decodeURIComponent(url.pathname.slice(prefix.length)) || 'index.html');
  if (existsSync(file) && statSync(file).isDirectory()) file = resolve(file, 'index.html');
  if (!file.startsWith(resolve(root, 'site') + '/') && !file.startsWith(resolve(root, 'site') + '\\')) return `outside site: ${url.href}`;
  if (!existsSync(file)) return `missing file: ${relative(root, file)}`;
  if (url.hash) {
    const ids = [...readFileSync(file, 'utf8').matchAll(/\bid\s*=\s*["']([^"']+)["']/g)].map(m => decode(m[1]));
    if (!ids.includes(decodeURIComponent(url.hash.slice(1)))) return `missing id ${url.hash} in ${relative(root, file)}`;
  }
}
const website = 'https://ov-refund.github.io/OV-Refund/';

// CSS selector scanner: split only at top level, respecting functions, attributes
// and quoted strings. The last compound is the element being positioned.
function splitSelector(selector: string, delimiter: RegExp): string[] {
  let depth = 0, quote = '', start = 0; const parts: string[] = [];
  for (let i = 0; i < selector.length; i++) {
    const c = selector[i];
    if (quote) { if (c === quote && selector[i - 1] !== '\\') quote = ''; continue; }
    if (c === '"' || c === "'") { quote = c; continue; }
    if (c === '(' || c === '[') depth++;
    if (c === ')' || c === ']') depth--;
    if (!depth && delimiter.test(c)) { if (selector.slice(start, i).trim()) parts.push(selector.slice(start, i).trim()); start = i + 1; }
  }
  if (selector.slice(start).trim()) parts.push(selector.slice(start).trim());
  return parts;
}
// Classes that dialogs actually get (`x.className = '…'` on a created <dialog>).
const dialogClasses = new Set(front.flatMap(file => [...read(file).matchAll(/\b(\w+)\s*=\s*document\.createElement\('dialog'\)[\s\S]{0,300}?\b\1\.className\s*=\s*([^;]+);/g)]
  .flatMap(m => [...m[2].matchAll(/'([^']*)'/g)].flatMap(s => s[1].split(/\s+/)).filter(Boolean))));
function couldBeDialog(selector: string): boolean {
  return splitSelector(selector, /,/).some(s => {
    const subject = splitSelector(s, /[\s>+~]/).at(-1)!;
    if (/::[\w-]+/.test(subject)) return false; // pseudo-elements are separate boxes
    const type = subject.match(/^[\w-]+/)?.[0];
    if (type && type !== 'dialog') return false;
    // A class the app never puts on a <dialog> cannot select one.
    if ([...subject.replace(/\([^()]*\)/g, '').matchAll(/\.([\w-]+)/g)].some(c => !dialogClasses.has(c[1]))) return false;
    for (const m of subject.matchAll(/:(?:is|where)\(([^()]*)\)/g)) if (!couldBeDialog(m[1])) return false;
    return true; // class/attribute/universal selectors can also select a dialog
  });
}

// Minimal CSS walker (styles.css uses no nesting syntax): every rule with a
// `position` declaration, and whether it sits outside any at-rule.
type PositionRule = { selector: string; position: string; topLevel: boolean };
function cssRules(css: string): PositionRule[] {
  const result: PositionRule[] = [], stack: string[] = [];
  let start = 0;
  css = css.replace(/\/\*[\s\S]*?\*\//g, '');
  for (let i = 0; i < css.length; i++) {
    if (css[i] === '{') {
      const prelude = css.slice(start, i).trim();
      if (prelude.startsWith('@')) { stack.push(prelude); start = i + 1; continue; }
      const end = css.indexOf('}', i);
      for (const m of css.slice(i + 1, end).matchAll(/(?:^|;)\s*position\s*:\s*([^;]+)/g))
        result.push({ selector: prelude.replace(/\s+/g, ' '), position: m[1].trim(), topLevel: stack.length === 0 });
      i = end; start = end + 1;
    } else if (css[i] === '}') { stack.pop(); start = i + 1; }
    else if (css[i] === ';' && css.slice(start, i).trim().startsWith('@')) start = i + 1; // @import etc.
  }
  return result;
}
// Approximate specificity [ids, classes/attributes/pseudo-classes, types].
function specificity(selector: string): number[] {
  const s = selector.replace(/:(?:is|where|not|has)\([^()]*\)/g, m => m.startsWith(':where') ? '' : ' .x').replace(/::[\w-]+/g, ' t');
  return [(s.match(/#[\w-]+/g) ?? []).length, (s.match(/\.[\w-]+|\[[^\]]*\]|:[\w-]+/g) ?? []).length,
    (s.match(/(?:^|[\s>+~])[a-z][\w-]*/gi) ?? []).length];
}
const compare = (a: number[], b: number[]) => a[0] - b[0] || a[1] - b[1] || a[2] - b[2];

describe('offline source regressions', () => {
  it('keeps navigation paths in the language of real UI labels', () => {
    const texts = front.flatMap(bilingual);
    for (const file of ['crates/ovrefund-core/src/email.rs', ...files('src-tauri/src', '.rs', true)]) {
      const raw = read(file).split('#[cfg(test)]')[0];
      for (const m of raw.matchAll(/"((?:\\.|[^"\\])*)"/g)) {
        if (!m[1].includes('→') || /\{[^}]*\}/.test(m[1])) continue; // interpolated train routes
        const halves = m[1].split(' / ');
        halves.forEach((text, i) => texts.push({ file, offset: m.index, text, lang: halves.length === 2 ? i ? 'en' : 'nl' : undefined }));
      }
    }
    texts.push(...pages.flatMap(htmlTexts));
    for (const file of ['README.md', 'SECURITY.md']) read(file).split('\n').reduce((offset, text) => {
      texts.push({ file, offset, text }); return offset + text.length + 1;
    }, 0);
    expect(texts.flatMap(navigation)).toEqual([]);
    expect(checkedPaths, 'navigation paths found').toBeGreaterThan(25);
    expect(ui.nl.has('Jouw gegevens') && ui.en.has('Your details')).toBe(true);
    expect(navigation({ file: 'README.md', offset: 0, lang: 'en', text: 'Settings → Meldingen' })).toHaveLength(1);
  });

  it('keeps frontend external destinations within the backend policy and real site targets', () => {
    const policy = read('src-tauri/src/commands.rs').match(/fn external_url_ok[\s\S]*?const ALLOWED:[^=]+= &\[([\s\S]*?)\];/)![1];
    const allowed = [...policy.matchAll(/"([^"]+)"/g)].map(m => m[1]);
    const errors: string[] = [];
    const constants = new Map<string, string>();
    for (const file of front) visit(source(file), n => {
      if (ts.isVariableDeclaration(n) && ts.isIdentifier(n.name)) { const s = literal(n.initializer); if (s?.startsWith('https://')) constants.set(n.name.text, s); }
    });
    const targets: Text[] = [];
    for (const file of front) {
      const raw = read(file);
      // Include URL literals (also provider help constants) and expanded hrefs.
      for (const m of raw.matchAll(/https?:\/\/[^\s'"`<>]+/g)) {
        // This endpoint is a POST relay, never an open_external destination.
        if (file === 'src/main.ts' && m[0] === 'https://api.web3forms.com/submit') continue;
        targets.push({ file, offset: m.index, text: m[0] });
      }
      for (const m of raw.matchAll(/(?:href|data-url)=["']([^"']+)["']/g)) {
        const expanded = m[1].replace(/\$\{(\w+)\}/g, (_, key) => constants.get(key) ?? `\${${key}}`);
        if (!expanded.includes('${') && /^https?:/.test(expanded)) {
          targets.push({ file, offset: m.index, text: expanded });
          const linkText = raw.slice(m.index, raw.indexOf('</a>', m.index));
          if (/Full disclaimer|Volledige disclaimer/.test(linkText) && !expanded.endsWith('disclaimer.html')) errors.push(`${file}: full disclaimer must end in disclaimer.html`);
        }
      }
      // Resolve WEBSITE_URL + 'page#id', as well as template hrefs above.
      for (const m of raw.matchAll(/\b(\w+)\s*\+\s*['"]([^'"]+)['"]/g)) if (constants.has(m[1])) targets.push({ file, offset: m.index, text: constants.get(m[1])! + m[2] });
    }
    expect(targets.length).toBeGreaterThan(5);
    for (const t of targets) {
      const url = new URL(decode(t.text));
      if (url.protocol !== 'https:' || !allowed.includes(url.hostname)) errors.push(`${at(t)}: backend disallows ${url.href}`);
      if (url.hostname === 'github.com' && !url.pathname.toLowerCase().startsWith('/ov-refund/')) errors.push(`${at(t)}: GitHub outside /ov-refund/: ${url.href}`);
      if (url.hostname === 'ov-refund.github.io') { const error = checkSiteTarget(url); if (error) errors.push(`${at(t)}: ${error}`); }
    }
    expect(errors).toEqual([]);
  });

  it('resolves every website href locally and limits external hosts', () => {
    const external = ['github.com', 'gtbv-formulier.ns.nl', 'www.ns.nl', 'www.nsinternational.com', 'developer.microsoft.com'];
    const errors: string[] = [];
    for (const file of pages) for (const m of read(file).matchAll(/\bhref\s*=\s*["']([^"']*)["']/g)) {
      const url = new URL(decode(m[1]), website + file.slice(5));
      if (url.origin === new URL(website).origin) {
        const error = checkSiteTarget(url); if (error) errors.push(`${at({ file, offset: m.index, text: '' })}: ${error}`);
      } else if (url.protocol !== 'https:' || !external.includes(url.hostname)) errors.push(`${file}: unexpected external href ${url.href}`);
    }
    expect(errors).toEqual([]);
  });

  it('keeps open modal dialogs fixed after rules that could position them', () => {
    const rules = cssRules(read('src/styles.css'));
    const guards = rules.filter(r => r.topLevel && /^(?:dialog:modal|dialog\[open\])$/.test(r.selector) && /^fixed(\s*!important)?$/.test(r.position));
    const important = (p: string) => p.includes('!important');
    const errors: string[] = [];
    for (const [index, rule] of rules.entries()) {
      if (!/^(relative|absolute)(\s*!important)?$/.test(rule.position)) continue;
      for (const selector of splitSelector(rule.selector, /,/).filter(s => couldBeDialog(s))) {
        // A guard wins when it is !important, or more specific, or equally specific and later.
        const beaten = guards.some(g => {
          if (important(g.position) !== important(rule.position)) return important(g.position);
          const d = compare(specificity(g.selector), specificity(selector));
          return d > 0 || (d === 0 && rules.indexOf(g) > index);
        });
        if (!beaten) errors.push(`src/styles.css: "${selector}" sets position: ${rule.position} on what could be an open modal dialog`);
      }
    }
    expect(guards.length, 'dialog:modal { position: fixed } guard').toBeGreaterThan(0);
    expect(errors).toEqual([]);
    expect(couldBeDialog(':is(.card, dialog)')).toBe(true);
    expect(couldBeDialog('dialog .hd::after')).toBe(false);
    expect(couldBeDialog('dialog .hd')).toBe(false); // .hd is a header inside the dialog
    expect(couldBeDialog('dialog.review')).toBe(true);
    expect(couldBeDialog('dialog > button')).toBe(false);
    expect(couldBeDialog('.note.warn')).toBe(false);
    expect(couldBeDialog('.review')).toBe(true);
    expect([...dialogClasses]).toEqual(expect.arrayContaining(['settings', 'bugdlg', 'review', 'ask']));
    expect(compare(specificity('dialog:modal'), specificity('.a.b'))).toBeLessThan(0);
    expect(cssRules('@media (x) { dialog { position: relative } } p { color: red }')).toEqual([{ selector: 'dialog', position: 'relative', topLevel: false }]);
  });
});
