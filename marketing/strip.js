// Fills the Stream Deck + touch strip mockup with the same timers the keys show.
(function () {
  var dd = "https://ddragon.leagueoflegends.com/cdn/16.22.1/img/";
  var timers = [
    ["MonkeyKing", "SummonerFlash", "Wukong Flash", "4:24", 88],
    ["LeeSin", "SummonerFlash", "Lee Sin Flash", "4:52", 97],
    ["Nunu", "SummonerFlash", "Nunu Flash", "4:53", 98],
    ["MonkeyKing", "SummonerTeleport", "Wukong Quest TP", "5:48", 99],
  ];
  var strip = document.getElementById("strip");
  if (!strip) return;
  timers.forEach(function (t) {
    var seg = document.createElement("div");
    seg.className = "seg";
    seg.innerHTML =
      '<div class="in">' +
      '<img class="champ" src="' + dd + "champion/" + t[0] + '.png">' +
      '<img class="spell" src="' + dd + "spell/" + t[1] + '.png">' +
      '<div class="name">' + t[2] + "</div>" +
      '<div class="time">' + t[3] + "</div>" +
      '<div class="bar"><i style="width:' + t[4] + '%"></i></div>' +
      "</div>";
    strip.appendChild(seg);
  });
  // Scale the 200x100 layout to the segment width.
  requestAnimationFrame(function () {
    document.querySelectorAll(".seg").forEach(function (s) {
      s.querySelector(".in").style.transform = "scale(" + s.clientWidth / 200 + ")";
    });
  });
})();
