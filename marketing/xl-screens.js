// Individual LCD interiors measured on the untouched official XL photo.
// A single projection of a flat 8x4 grid distorted its curved rows and bezels.
(function () {
  const rows = [
    { y: 552, bottom: 631, xs: [324, 474, 623, 768, 916, 1061, 1209, 1353] },
    { y: 666, bottom: 741, xs: [333, 478, 625, 770, 915, 1060, 1204, 1347] },
    { y: 777, bottom: 849, xs: [341, 485, 629, 772, 916, 1060, 1203, 1345] },
    { y: 885, bottom: 955, xs: [350, 491, 634, 777, 918, 1059, 1201, 1343] }
  ];
  const keys = ["k0", "k1", "k2", "k3", "xl_enemy5", "insight", "reset", null,
    "xl_timer1", "xl_timer2", "xl_timer3", "xl_timer4"];
  const root = document.getElementById("xl-screens");
  rows.forEach((row, r) => row.xs.forEach((x, c) => {
    const el = document.createElement("div");
    el.className = "k";
    el.setAttribute("data-quad", JSON.stringify([[x, row.y], [x + 110, row.y], [x + 110, row.bottom], [x, row.bottom]]));
    if (keys[r * 8 + c]) {
      const img = document.createElement("img");
      img.src = "keys/" + keys[r * 8 + c] + ".svg";
      el.appendChild(img);
    }
    root.appendChild(el);
  }));
})();
