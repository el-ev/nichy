const $ = s => document.querySelector(s);

function mk(tag, cls, txt) {
  const e = document.createElement(tag);
  if (cls) e.className = cls;
  if (txt != null) e.textContent = txt;
  return e;
}

function esc(s) {
  return String(s).replace(/&/g, '&amp;').replace(/</g, '&lt;')
    .replace(/>/g, '&gt;').replace(/"/g, '&quot;');
}

function updateThemeBtn() {
  const isDark = document.documentElement.dataset.theme !== 'light';
  $('#themeBtn').textContent = isDark ? 'light' : 'dark';
}

function toggleTheme() {
  const html = document.documentElement;
  html.dataset.theme = html.dataset.theme === 'dark' ? 'light' : 'dark';
  localStorage.setItem('nichy-theme', html.dataset.theme);
  updateThemeBtn();
}

(function() {
  const saved = localStorage.getItem('nichy-theme');
  if (saved) document.documentElement.dataset.theme = saved;
  updateThemeBtn();
})();
