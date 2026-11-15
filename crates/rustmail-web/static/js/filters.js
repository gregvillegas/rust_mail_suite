// Filters page behaviour: the advanced Sieve editor.
//
// Like app.js this works with Alpine's CSP build: the component is named and
// registered here, templates only reference it. No eval, no inline code.
//
// The page works without any of this: the script is a plain <textarea>. When the
// vendored CodeMirror bundle loads, it replaces the textarea with an editor that
// writes every change back into it, so the form submits the same field either way.
// The bundle is loaded on demand (hx-boost swaps pages without running <script> tags).
(function () {
  'use strict';

  var bundle = null;

  function loadEditor(src) {
    if (window.RustMailSieveEditor) return Promise.resolve(window.RustMailSieveEditor);
    if (!bundle) {
      bundle = new Promise(function (resolve, reject) {
        var tag = document.createElement('script');
        tag.src = src;
        tag.onload = function () { resolve(window.RustMailSieveEditor); };
        tag.onerror = function () { bundle = null; reject(new Error('editor did not load')); };
        document.head.appendChild(tag);
      });
    }
    return bundle;
  }

  document.addEventListener('alpine:init', function () {
    window.Alpine.data('sieveEditor', function () {
      return {
        editor: null,
        init: function () {
          var self = this;
          var source = this.$refs.source;
          var src = this.$el.getAttribute('data-editor-src');
          loadEditor(src).then(function (api) {
            // The page may have been swapped out while the bundle was loading.
            if (!source.isConnected) return;
            self.editor = api.create(source);
            self.$el.setAttribute('data-editor-ready', 'true');
          }).catch(function () {
            self.$el.setAttribute('data-editor-ready', 'false');
          });
        },
        destroy: function () {
          if (this.editor) this.editor.destroy();
          this.editor = null;
        }
      };
    });
  });
})();
