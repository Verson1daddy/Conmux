// 中英双语切换：/zh/ ↔ /en/ 同路径互跳（两本书页面一一对应）。
// 本地单书构建（无 /zh|en/ 前缀）时不显示按钮。
(function () {
  var m = location.pathname.match(/^(.*)\/(zh|en)\//);
  if (!m) return;
  var other = m[2] === "zh" ? "en" : "zh";
  var target = location.pathname.replace("/" + m[2] + "/", "/" + other + "/");
  var bar = document.querySelector(".right-buttons");
  if (!bar) return;
  var a = document.createElement("a");
  a.href = target + location.hash;
  a.title = other === "en" ? "Switch to English" : "切换到中文";
  a.textContent = other === "en" ? "EN" : "中文";
  a.style.cssText = "display:inline-block;padding:0 8px;font-size:1.4rem;line-height:2;text-decoration:none;font-weight:600;";
  bar.insertBefore(a, bar.firstChild);
})();
