// Places flat key images onto an official Elgato product photo.
// Each overlay is laid out flat, then mapped onto four corner points measured
// on the photo (in the photo's own pixels), so it follows the camera angle.
(function () {
  // Solves for the 3x3 projection that maps the rectangle (0,0)-(w,h) onto
  // the quad [tl, tr, br, bl] and returns it as a CSS matrix3d.
  function quadMatrix(w, h, q) {
    var src = [[0, 0], [w, 0], [w, h], [0, h]];
    var A = [], b = [];
    for (var i = 0; i < 4; i++) {
      var x = src[i][0], y = src[i][1], X = q[i][0], Y = q[i][1];
      A.push([x, y, 1, 0, 0, 0, -x * X, -y * X]); b.push(X);
      A.push([0, 0, 0, x, y, 1, -x * Y, -y * Y]); b.push(Y);
    }
    // Gaussian elimination.
    for (var c = 0; c < 8; c++) {
      var p = c;
      for (var r = c + 1; r < 8; r++) if (Math.abs(A[r][c]) > Math.abs(A[p][c])) p = r;
      var t = A[c]; A[c] = A[p]; A[p] = t; t = b[c]; b[c] = b[p]; b[p] = t;
      for (r = c + 1; r < 8; r++) {
        var f = A[r][c] / A[c][c];
        for (var k = c; k < 8; k++) A[r][k] -= f * A[c][k];
        b[r] -= f * b[c];
      }
    }
    var hv = new Array(8);
    for (c = 7; c >= 0; c--) {
      var s = b[c];
      for (k = c + 1; k < 8; k++) s -= A[c][k] * hv[k];
      hv[c] = s / A[c][c];
    }
    var H = hv.concat([1]);
    return "matrix3d(" + [H[0], H[3], 0, H[6], H[1], H[4], 0, H[7], 0, 0, 1, 0, H[2], H[5], 0, H[8]].join(",") + ")";
  }

  document.querySelectorAll("[data-quad]").forEach(function (el) {
    var q = JSON.parse(el.getAttribute("data-quad"));
    el.style.transform = quadMatrix(el.offsetWidth, el.offsetHeight, q);
  });
})();
