// Render captured setFeedback values using the actual layout.json geometry.
(function () {
  const strip = document.getElementById("strip");
  if (!strip) return;
  const capture = window.galleryCapture;
  if (!capture) throw new Error("Missing plugin capture. Run tools/build-gallery.ps1.");
  for (let n = 0; n < 4; n++) {
    const feedback = capture.feedback["dial" + n];
    if (!feedback) throw new Error("Missing dial " + n);
    const segment = document.createElement("div");
    segment.className = "captured-segment";
    segment.style.left = (n * 200) + "px";
    for (const item of capture.layout.items) {
      const el = document.createElement(item.type === "pixmap" ? "img" : "div");
      const [x, y, width, height] = item.rect;
      Object.assign(el.style, { position: "absolute", left: x + "px", top: y + "px", width: width + "px", height: height + "px", overflow: "hidden" });
      if (item.type === "pixmap") {
        el.src = feedback[item.key];
      } else if (item.type === "text") {
        el.textContent = feedback[item.key];
        Object.assign(el.style, { fontFamily: "Arial", fontSize: item.font.size + "px", fontWeight: item.font.weight, whiteSpace: "nowrap", textOverflow: "ellipsis", textAlign: item.alignment });
      } else if (item.type === "bar") {
        Object.assign(el.style, { background: item.bar_bg_c, border: item.border_w + "px solid " + item.bar_border_c });
        const fill = document.createElement("div");
        Object.assign(fill.style, { width: feedback[item.key].value + "%", height: "100%", background: item.bar_fill_c });
        el.appendChild(fill);
      }
      segment.appendChild(el);
    }
    strip.appendChild(segment);
  }
})();
