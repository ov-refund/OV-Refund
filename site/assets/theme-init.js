// Runs in <head> before the page is drawn, so the right theme shows without a flash.
// Your own choice (toggle) wins; otherwise follow the browser/Windows setting; light if there is none.
(function () {
  var root = document.documentElement;
  var saved = null;
  try { saved = localStorage.getItem('theme'); } catch (e) { /* storage blocked */ }
  var mq = window.matchMedia ? window.matchMedia('(prefers-color-scheme: dark)') : null;
  var apply = function () {
    root.setAttribute('data-theme', saved === 'light' || saved === 'dark' ? saved : (mq && mq.matches ? 'dark' : 'light'));
  };
  apply();
  // follow live changes of the system setting until the visitor picks a theme themselves
  if (mq && mq.addEventListener && saved !== 'light' && saved !== 'dark') {
    mq.addEventListener('change', function () {
      try { saved = localStorage.getItem('theme'); } catch (e) { /* ignore */ }
      if (saved !== 'light' && saved !== 'dark') {
        apply();
        document.dispatchEvent(new Event('themechange'));
      }
    });
  }
})();
