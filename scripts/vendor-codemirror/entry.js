// Entry point of the vendored editor bundle: a CodeMirror 6 editor for Sieve scripts
// that stands in for a <textarea> and writes every change back into it.
import { EditorState } from '@codemirror/state';
import { EditorView, keymap, lineNumbers, highlightActiveLine, drawSelection } from '@codemirror/view';
import { defaultKeymap, history, historyKeymap, indentWithTab } from '@codemirror/commands';
import { StreamLanguage, syntaxHighlighting, defaultHighlightStyle, bracketMatching, indentOnInput } from '@codemirror/language';
import { sieve } from '@codemirror/legacy-modes/mode/sieve';

function create(textarea) {
  const host = document.createElement('div');
  host.className = 'rm-editor form-control p-0';
  host.setAttribute('data-testid', 'sieve-editor');
  textarea.insertAdjacentElement('afterend', host);
  // The editor lives in a shadow root on purpose. CodeMirror styles itself by adding a
  // <style> element, which RustMail's Content-Security-Policy (style-src 'self') forbids.
  // In a shadow root it uses constructable stylesheets instead, which that policy allows,
  // so the strict policy does not have to be relaxed for one page.
  const shadow = host.attachShadow({ mode: 'open' });
  const label = document.querySelector('label[for="' + textarea.id + '"]');

  const view = new EditorView({
    parent: shadow,
    root: shadow,
    state: EditorState.create({
      doc: textarea.value,
      extensions: [
        lineNumbers(),
        history(),
        drawSelection(),
        EditorView.theme({
          '&': { height: '100%', fontSize: '0.9rem' },
          '.cm-scroller': { overflow: 'auto', fontFamily: 'var(--bs-font-monospace, monospace)' },
          '&.cm-focused': { outline: '2px solid #0d6efd', outlineOffset: '-2px' },
        }),
        highlightActiveLine(),
        indentOnInput(),
        bracketMatching(),
        StreamLanguage.define(sieve),
        syntaxHighlighting(defaultHighlightStyle, { fallback: true }),
        // Tab moves focus out, as people expect from a form field; Ctrl/Cmd-] indents.
        keymap.of([...defaultKeymap, ...historyKeymap, indentWithTab]),
        EditorView.contentAttributes.of({
          'aria-label': label ? label.textContent.trim() : 'Sieve script',
          'aria-multiline': 'true',
          spellcheck: 'false',
          autocapitalize: 'none',
        }),
        EditorView.updateListener.of((update) => {
          if (update.docChanged) textarea.value = update.state.doc.toString();
        }),
      ],
    }),
  });
  textarea.hidden = true;
  return {
    destroy() {
      view.destroy();
      host.remove();
      textarea.hidden = false;
    },
  };
}

window.RustMailSieveEditor = { create };
