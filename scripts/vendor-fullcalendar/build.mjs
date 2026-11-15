// Bundles entry.js and FullCalendar into one minified script plus one stylesheet, and writes
// them, with the licence texts, to crates/rustmail-web/static/vendor/fullcalendar/6/.
//
//   cd scripts/vendor-fullcalendar && npm ci && npm run build
//
// The output is committed; this is only needed to rebuild or upgrade it.
//
// FullCalendar normally adds its CSS to the page by creating a <style> element at run time.
// RustMail's Content-Security-Policy (`style-src 'self'`) forbids that, and the policy is not
// relaxed. So the build takes each stylesheet out of the library's JavaScript (it is a string
// followed by an `injectStyles(...)` call), writes them together into fullcalendar.css, which
// the page links like any other stylesheet, and removes the calls. The build fails if the
// library's layout changes, so an upgrade cannot silently bring the problem back.
import { build } from 'esbuild';
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { createHash } from 'node:crypto';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const here = path.dirname(fileURLToPath(import.meta.url));
const out = path.resolve(here, '../../crates/rustmail-web/static/vendor/fullcalendar/6');
mkdirSync(out, { recursive: true });

const EXPECTED_STYLESHEETS = 4; // core, daygrid, timegrid, list
const stylesheets = [];
const pattern = /var css_248z = ("(?:[^"\\]|\\.)*");\s*injectStyles\(css_248z\);/;

const stylesToFile = {
  name: 'styles-to-file',
  setup(b) {
    b.onLoad({ filter: /@fullcalendar[\\/](core[\\/]internal-common|daygrid[\\/]internal|timegrid[\\/]internal|list[\\/]internal)\.js$/ }, (args) => {
      const source = readFileSync(args.path, 'utf8');
      const match = pattern.exec(source);
      if (!match) throw new Error(`no injectStyles(css) found in ${args.path}: review the CSP step in build.mjs`);
      stylesheets.push({ file: path.relative(here, args.path), css: JSON.parse(match[1]) });
      let patched = source.replace(pattern, '/* stylesheet moved to fullcalendar.css */');
      if (args.path.endsWith('internal-common.js')) {
        // With the CSS in a linked file there is nothing left to put in the <style> element
        // FullCalendar creates at start-up, and `style-src` refuses the (empty) element itself.
        const creator = 'function registerStylesRoot(rootNode) {';
        if (patched.split(creator).length !== 2 || !patched.includes("createElement('style')")) {
          throw new Error('FullCalendar no longer creates its style element the way build.mjs expects: review the CSP step');
        }
        patched = patched.replace(creator, `${creator}\n    return; // the stylesheet is linked from the page (see build.mjs)`);
      }
      return { contents: patched, loader: 'js' };
    });
  },
};

// Preact (FullCalendar's renderer) applies a style given as a string with `style.cssText = "..."`,
// which `style-src 'self'` blocks; setting properties one by one is allowed. Rewrite both uses.
const cspSafePreact = {
  name: 'csp-safe-preact',
  setup(b) {
    b.onLoad({ filter: /preact[\\/]dist[\\/]preact\.module\.js$/ }, (args) => {
      let source = readFileSync(args.path, 'utf8');
      const set = 'n.style.cssText=u';
      const clear = 'n.style.cssText=i=""';
      if (source.split(set).length !== 2 || source.split(clear).length !== 2) {
        throw new Error('preact no longer assigns style.cssText the way build.mjs expects: review the CSP step');
      }
      source = source.replace(set, 'rmStyle(n.style,u)').replace(clear, '(rmStyle(n.style,""),i="")');
      const helper = 'function rmStyle(s,t){for(;s.length;)s.removeProperty(s.item(0));for(const p of String(t).split(";")){const i=p.indexOf(":");i>0&&s.setProperty(p.slice(0,i).trim(),p.slice(i+1).trim())}}';
      return { contents: `${helper}\n${source}`, loader: 'js' };
    });
  },
};

await build({
  entryPoints: [path.join(here, 'entry.js')],
  bundle: true,
  minify: true,
  format: 'iife',
  target: ['es2020'],
  legalComments: 'none',
  plugins: [stylesToFile, cspSafePreact],
  outfile: path.join(out, 'calendar.js'),
});

if (stylesheets.length !== EXPECTED_STYLESHEETS) {
  throw new Error(`expected ${EXPECTED_STYLESHEETS} stylesheets, found ${stylesheets.length}: ${stylesheets.map((s) => s.file).join(', ')}`);
}
// Core first (the others build on its variables), then the views.
stylesheets.sort((a, b) => (b.file.includes('core') ? 1 : 0) - (a.file.includes('core') ? 1 : 0) || a.file.localeCompare(b.file));
let css = stylesheets.map((s) => `/* ${s.file} */\n${s.css}\n`).join('\n');
// The icon font (the arrows in the toolbar) is embedded as a data: URI. The policy's `font-src`
// does not allow data:, so it is written out as a real file next to the stylesheet.
const fontPattern = /url\("data:application\/x-font-ttf;charset=utf-8;base64,([A-Za-z0-9+/=]+)"\)/g;
const fonts = [...css.matchAll(fontPattern)];
if (fonts.length !== 1) throw new Error(`expected one embedded font, found ${fonts.length}: review the CSP step in build.mjs`);
writeFileSync(path.join(out, 'fcicons.ttf'), Buffer.from(fonts[0][1], 'base64'));
css = css.replace(fontPattern, 'url("fcicons.ttf")');
if (css.includes('data:')) throw new Error('the stylesheet still contains a data: URI');
writeFileSync(path.join(out, 'calendar.css'), css);
if (/\.style\.cssText\s*=/.test(readFileSync(path.join(out, 'calendar.js'), 'utf8'))) {
  console.warn('note: the bundle assigns style.cssText somewhere; check the browser console for CSP errors');
}

const packages = ['@fullcalendar/core', '@fullcalendar/daygrid', '@fullcalendar/timegrid', '@fullcalendar/list', '@fullcalendar/interaction', 'preact'];
let notice = 'The calendar bundle (calendar.js, calendar.css) contains these MIT-licensed packages.\n\n';
for (const name of packages) {
  const dir = path.join(here, 'node_modules', name);
  let version = '?';
  let text = '';
  for (const file of ['LICENSE.md', 'LICENSE']) {
    try { text = readFileSync(path.join(dir, file), 'utf8').trim(); break; } catch { /* try the next name */ }
  }
  try { version = JSON.parse(readFileSync(path.join(dir, 'package.json'), 'utf8')).version; } catch { continue; }
  notice += `=== ${name} ${version} ===\n${text}\n\n`;
}
writeFileSync(path.join(out, 'LICENSES.txt'), notice);

const sha = (f) => createHash('sha384').update(readFileSync(path.join(out, f))).digest('base64');
console.log(`wrote calendar.js sha384-${sha('calendar.js')}\nwrote calendar.css sha384-${sha('calendar.css')}`);
