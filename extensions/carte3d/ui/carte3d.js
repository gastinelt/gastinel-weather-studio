/*
 * Carte 3D — rendu WebGL du relief et des points du projet.
 *
 * Le panneau vit dans une iframe (origine gwsext) sans accès à l'API Tauri :
 * il parle à l'application par postMessage. L'app relaie vers la DLL
 * (`extension_call`) et renvoie la réponse.
 *
 * Repère : x vers l'est, y vers le haut, z vers le sud, en kilomètres.
 */
(() => {
  "use strict";

  const COULEURS = {
    Cat1: "#9aa4b1", Cat2: "#4a90d9", Cat3: "#1e4d8b", Cat4: "#5b2d8e",
    Col: "#c47d22", Versants: "#4c8066",
  };
  const LIMITE_ETIQUETTES = 80;
  /** Densité de pixels maximale du rendu WebGL. */
  const DPR_MAX = 1.5;
  /** Déplacement au clic droit : +35 % par rapport à la 0.1.0. */
  const SENSIBILITE_DEPLACEMENT = 1.35;

  // --- Pont avec l'application ---------------------------------------------
  const attentes = new Map();
  let prochain = 1;
  function appeler(method, payload) {
    return new Promise((ok, ko) => {
      const rid = prochain++;
      attentes.set(rid, { ok, ko });
      parent.postMessage({ gws: "call", rid, method, payload }, "*");
    });
  }
  window.addEventListener("message", (ev) => {
    if (ev.source !== parent) return;
    const m = ev.data || {};
    if (m.gws === "reply" && attentes.has(m.rid)) {
      const a = attentes.get(m.rid);
      attentes.delete(m.rid);
      m.err ? a.ko(new Error(m.err)) : a.ok(m.ok);
    } else if (m.gws === "context") {
      demarrer(m.project_id).catch((e) => statut("Impossible d'afficher la carte 3D : " + e.message));
    }
  });
  parent.postMessage({ gws: "ready" }, "*");

  const $ = (id) => document.getElementById(id);
  /**
   * Fenêtre de chargement au premier plan. Le calcul côté DLL est d'un seul
   * tenant : pas de pourcentage réel, donc des étapes indicatives qui défilent.
   */
  let minuterieEtapes = null;
  function chargement(actif) {
    const boite = $("chargement");
    clearInterval(minuterieEtapes);
    boite.hidden = !actif;
    if (!actif) return;
    statut("");
    const etapes = ["Lecture des points du projet…", "Téléchargement du relief LiDAR HD (IGN)…",
      "Téléchargement du fond Plan IGN…", "Construction du relief 3D…",
      "Grand projet : l'IGN peut prendre quelques secondes de plus…"];
    let i = 0;
    $("chargement-texte").textContent = etapes[0];
    minuterieEtapes = setInterval(() => {
      i = Math.min(i + 1, etapes.length - 1);
      $("chargement-texte").textContent = etapes[i];
    }, 2200);
  }

  function statut(t) {
    $("statut").hidden = !t;
    $("statut").textContent = t || "";
  }

  // --- Mathématiques ---------------------------------------------------------
  const M = {
    perspective(fov, asp, n, f) {
      const t = 1 / Math.tan(fov / 2), nf = 1 / (n - f);
      return [t / asp, 0, 0, 0, 0, t, 0, 0, 0, 0, (f + n) * nf, -1, 0, 0, 2 * f * n * nf, 0];
    },
    lookAt(e, c, u) {
      let z = norm(sub(e, c)), x = norm(cross(u, z)), y = cross(z, x);
      return [x[0], y[0], z[0], 0, x[1], y[1], z[1], 0, x[2], y[2], z[2], 0,
        -dot(x, e), -dot(y, e), -dot(z, e), 1];
    },
    mul(a, b) {
      const r = new Array(16);
      for (let i = 0; i < 4; i++) for (let j = 0; j < 4; j++) {
        let s = 0;
        for (let k = 0; k < 4; k++) s += a[k * 4 + j] * b[i * 4 + k];
        r[i * 4 + j] = s;
      }
      return r;
    },
  };
  const sub = (a, b) => [a[0] - b[0], a[1] - b[1], a[2] - b[2]];
  const dot = (a, b) => a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
  const cross = (a, b) => [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]];
  const norm = (a) => { const l = Math.hypot(a[0], a[1], a[2]) || 1; return [a[0] / l, a[1] / l, a[2] / l]; };
  const hex = (h) => [1, 3, 5].map((i) => parseInt(h.slice(i, i + 2), 16) / 255);

  // --- WebGL -----------------------------------------------------------------
  const canvas = $("gl");
  // Sans multi-échantillonnage : le tampon MSAA coûtait autant que l'image
  // elle-même, pour un gain à peine visible sur un relief texturé.
  const gl = canvas.getContext("webgl", { antialias: false });
  if (!gl) { statut("WebGL n'est pas disponible sur ce poste."); return; }
  gl.getExtension("OES_element_index_uint");

  function programme(vs, fs) {
    const p = gl.createProgram();
    for (const [type, src] of [[gl.VERTEX_SHADER, vs], [gl.FRAGMENT_SHADER, fs]]) {
      const s = gl.createShader(type);
      gl.shaderSource(s, src);
      gl.compileShader(s);
      if (!gl.getShaderParameter(s, gl.COMPILE_STATUS)) throw new Error(gl.getShaderInfoLog(s));
      gl.attachShader(p, s);
    }
    gl.linkProgram(p);
    return p;
  }

  // Relief : teinte hypsométrique + ombrage (lumière du nord-ouest).
  const progRelief = programme(`
    attribute vec3 aPos; attribute vec3 aNorm; attribute float aAlt;
    uniform mat4 uMVP; uniform float uExag; uniform vec2 uTaillePlan;
    varying vec3 vNorm; varying float vAlt; varying vec2 vUV;
    void main() {
      vNorm = normalize(vec3(aNorm.x, aNorm.y / uExag, aNorm.z));
      vAlt = aAlt;
      // Image topo : x d'ouest en est, v = 0 au nord (première ligne de l'image).
      vUV = vec2(aPos.x / uTaillePlan.x + 0.5, aPos.z / uTaillePlan.y + 0.5);
      gl_Position = uMVP * vec4(aPos.x, aPos.y * uExag, aPos.z, 1.0);
    }`, `
    precision mediump float;
    varying vec3 vNorm; varying float vAlt; varying vec2 vUV;
    uniform float uMin; uniform float uMax;
    uniform sampler2D uTopo; uniform float uAvecTopo;
    vec3 teinte(float t) {
      vec3 c0 = vec3(0.36, 0.52, 0.34), c1 = vec3(0.62, 0.66, 0.42), c2 = vec3(0.60, 0.50, 0.38),
           c3 = vec3(0.55, 0.52, 0.50), c4 = vec3(0.96, 0.96, 0.97);
      if (t < 0.25) return mix(c0, c1, t / 0.25);
      if (t < 0.55) return mix(c1, c2, (t - 0.25) / 0.30);
      if (t < 0.85) return mix(c2, c3, (t - 0.55) / 0.30);
      return mix(c3, c4, (t - 0.85) / 0.15);
    }
    void main() {
      float t = clamp((vAlt - uMin) / max(uMax - uMin, 1.0), 0.0, 1.0);
      vec3 l = normalize(vec3(-0.6, 0.9, -0.5));
      float d = clamp(dot(normalize(vNorm), l), 0.0, 1.0);
      if (uAvecTopo > 0.5) {
        // Plan IGN drapé, ombrage adouci pour garder la carte lisible.
        gl_FragColor = vec4(texture2D(uTopo, vUV).rgb * (0.6 + 0.5 * d), 1.0);
      } else {
        gl_FragColor = vec4(teinte(t) * (0.35 + 0.75 * d), 1.0);
      }
    }`);

  // Points et tiges : couleur par sommet.
  const progMarques = programme(`
    attribute vec3 aPos; attribute vec3 aCol;
    uniform mat4 uMVP; uniform float uExag; uniform float uTaille;
    varying vec3 vCol;
    void main() {
      vCol = aCol;
      gl_Position = uMVP * vec4(aPos.x, aPos.y * uExag, aPos.z, 1.0);
      gl_PointSize = uTaille;
    }`, `
    precision mediump float;
    varying vec3 vCol; uniform float uRond;
    void main() {
      if (uRond > 0.5) {
        vec2 c = gl_PointCoord - 0.5;
        float r = length(c);
        if (r > 0.5) discard;
        gl_FragColor = vec4(r > 0.36 ? vec3(1.0) : vCol, 1.0);
      } else {
        gl_FragColor = vec4(vCol, 1.0);
      }
    }`);

  let scene = null;

  function construire(data) {
    const t = data.terrain;
    const { nx, ny, z } = t;
    const L = t.width_km, H = t.height_km;
    const pos = new Float32Array(nx * ny * 3), alt = new Float32Array(nx * ny), nor = new Float32Array(nx * ny * 3);
    let zmin = Infinity, zmax = -Infinity;
    for (const v of z) { zmin = Math.min(zmin, v); zmax = Math.max(zmax, v); }
    const X = (i) => (i / (nx - 1) - 0.5) * L;
    const Z = (j) => (j / (ny - 1) - 0.5) * H; // j = 0 au nord
    for (let j = 0; j < ny; j++) for (let i = 0; i < nx; i++) {
      const k = j * nx + i;
      pos.set([X(i), z[k] / 1000, Z(j)], k * 3);
      alt[k] = z[k];
      const zl = z[j * nx + Math.max(i - 1, 0)], zr = z[j * nx + Math.min(i + 1, nx - 1)];
      const zu = z[Math.max(j - 1, 0) * nx + i], zd = z[Math.min(j + 1, ny - 1) * nx + i];
      const dx = (zr - zl) / 1000 / (X(Math.min(i + 1, nx - 1)) - X(Math.max(i - 1, 0)));
      const dz = (zd - zu) / 1000 / (Z(Math.min(j + 1, ny - 1)) - Z(Math.max(j - 1, 0)));
      nor.set(norm([-dx, 1, -dz]), k * 3);
    }
    const idx = new Uint32Array((nx - 1) * (ny - 1) * 6);
    let n = 0;
    for (let j = 0; j < ny - 1; j++) for (let i = 0; i < nx - 1; i++) {
      const a = j * nx + i, b = a + 1, c = a + nx, d = c + 1;
      idx.set([a, c, b, b, c, d], n); n += 6;
    }

    // Altitude du sol sous un point (interpolation bilinéaire).
    function sol(lat, lon) {
      const fi = (lon - t.west) / (t.east - t.west) * (nx - 1);
      const fj = (t.north - lat) / (t.north - t.south) * (ny - 1);
      const i = Math.max(0, Math.min(nx - 2, Math.floor(fi))), j = Math.max(0, Math.min(ny - 2, Math.floor(fj)));
      const u = Math.min(1, Math.max(0, fi - i)), v = Math.min(1, Math.max(0, fj - j));
      const q = (a, b) => z[b * nx + a];
      return (q(i, j) * (1 - u) + q(i + 1, j) * u) * (1 - v) + (q(i, j + 1) * (1 - u) + q(i + 1, j + 1) * u) * v;
    }

    const hauteurTige = Math.max(L, H) * 0.035;
    const points = data.points.map((p) => {
      const altitude = p.altitude_m ?? sol(p.lat, p.lon);
      return {
        ...p, altitude,
        x: ((p.lon - t.west) / (t.east - t.west) - 0.5) * L,
        z: ((t.north - p.lat) / (t.north - t.south) - 0.5) * H,
        y: altitude / 1000,
        couleur: hex(COULEURS[p.category] || "#6b7480"),
      };
    });

    const tiges = [], teintesTiges = [], tetes = [], teintesTetes = [];
    for (const p of points) {
      tiges.push(p.x, p.y, p.z, p.x, p.y + hauteurTige, p.z);
      teintesTiges.push(1, 1, 1, 1, 1, 1);
      tetes.push(p.x, p.y + hauteurTige, p.z);
      teintesTetes.push(...p.couleur);
    }

    const tampon = (donnees, cible = gl.ARRAY_BUFFER) => {
      const b = gl.createBuffer();
      gl.bindBuffer(cible, b);
      gl.bufferData(cible, donnees, gl.STATIC_DRAW);
      return b;
    };
    scene = {
      L, H, zmin, zmax, hauteurTige, points,
      relief: { pos: tampon(pos), nor: tampon(nor), alt: tampon(alt), idx: tampon(idx, gl.ELEMENT_ARRAY_BUFFER), n: idx.length },
      tiges: { pos: tampon(new Float32Array(tiges)), col: tampon(new Float32Array(teintesTiges)), n: tiges.length / 3 },
      tetes: { pos: tampon(new Float32Array(tetes)), col: tampon(new Float32Array(teintesTetes)), n: tetes.length / 3 },
    };
    scene.topo = null;
    if (data.texture) chargerTopo(data.texture);
    construireEtiquettes();
    recentrer();
  }

  /** Fond Plan IGN : texture non puissance de 2, donc sans mipmap et bords bloqués. */
  function chargerTopo(url) {
    const img = new Image();
    // Hôte trop ancien pour servir `_donnees/` : la DLL rend l'image en data URL.
    img.onerror = () => {
      if (url.startsWith("data:")) return;
      appeler("texture_data_url", { name: url.split("/").pop() })
        .then((d) => { img.onerror = null; img.src = d; })
        .catch(() => {});
    };
    img.onload = () => {
      const tex = gl.createTexture();
      gl.bindTexture(gl.TEXTURE_2D, tex);
      gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGB, gl.RGB, gl.UNSIGNED_BYTE, img);
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.LINEAR);
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.LINEAR);
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
      if (scene) scene.topo = tex;
      else gl.deleteTexture(tex);
      $("fond").disabled = false;
      dessiner();
    };
    img.src = url;
  }

  // --- Caméra orbitale -----------------------------------------------------
  const cam = { yaw: 0.35, pitch: 0.75, dist: 10, cible: [0, 0, 0] };
  let exag = 1.5;

  function recentrer() {
    if (!scene) return;
    cam.yaw = 0.35; cam.pitch = 0.75;
    cam.dist = Math.max(scene.L, scene.H) * 1.25;
    cam.cible = [0, ((scene.zmin + scene.zmax) / 2000) * exag, 0];
    dessiner();
  }

  function matrice() {
    const w = canvas.clientWidth, h = canvas.clientHeight;
    const e = [
      cam.cible[0] + cam.dist * Math.cos(cam.pitch) * Math.sin(cam.yaw),
      cam.cible[1] + cam.dist * Math.sin(cam.pitch),
      cam.cible[2] + cam.dist * Math.cos(cam.pitch) * Math.cos(cam.yaw),
    ];
    const proj = M.perspective(0.8, w / Math.max(h, 1), cam.dist * 0.01, cam.dist * 10);
    return M.mul(proj, M.lookAt(e, cam.cible, [0, 1, 0]));
  }

  function lier(prog, nom, tampon, taille) {
    const loc = gl.getAttribLocation(prog, nom);
    gl.bindBuffer(gl.ARRAY_BUFFER, tampon);
    gl.enableVertexAttribArray(loc);
    gl.vertexAttribPointer(loc, taille, gl.FLOAT, false, 0, 0);
  }

  let demande = false;
  function dessiner() {
    if (demande) return;
    demande = true;
    requestAnimationFrame(() => { demande = false; rendu(); });
  }

  function rendu() {
    // Densité plafonnée à 1,5 : sur un écran 2× ou 3×, le tampon de rendu
    // (couleur + profondeur) grossit au carré pour un gain peu visible.
    const dpr = Math.min(window.devicePixelRatio || 1, DPR_MAX);
    const w = Math.round(canvas.clientWidth * dpr), h = Math.round(canvas.clientHeight * dpr);
    if (canvas.width !== w || canvas.height !== h) { canvas.width = w; canvas.height = h; }
    gl.viewport(0, 0, w, h);
    gl.clearColor(0.06, 0.09, 0.125, 1);
    gl.clear(gl.COLOR_BUFFER_BIT | gl.DEPTH_BUFFER_BIT);
    if (!scene) return;
    gl.enable(gl.DEPTH_TEST);
    const mvp = matrice();

    gl.useProgram(progRelief);
    gl.uniformMatrix4fv(gl.getUniformLocation(progRelief, "uMVP"), false, mvp);
    gl.uniform1f(gl.getUniformLocation(progRelief, "uExag"), exag);
    gl.uniform1f(gl.getUniformLocation(progRelief, "uMin"), scene.zmin);
    gl.uniform1f(gl.getUniformLocation(progRelief, "uMax"), scene.zmax);
    gl.uniform2f(gl.getUniformLocation(progRelief, "uTaillePlan"), scene.L, scene.H);
    const avecTopo = !!scene.topo && $("fond").value === "topo";
    gl.uniform1f(gl.getUniformLocation(progRelief, "uAvecTopo"), avecTopo ? 1 : 0);
    if (avecTopo) {
      gl.activeTexture(gl.TEXTURE0);
      gl.bindTexture(gl.TEXTURE_2D, scene.topo);
      gl.uniform1i(gl.getUniformLocation(progRelief, "uTopo"), 0);
    }
    lier(progRelief, "aPos", scene.relief.pos, 3);
    lier(progRelief, "aNorm", scene.relief.nor, 3);
    lier(progRelief, "aAlt", scene.relief.alt, 1);
    gl.bindBuffer(gl.ELEMENT_ARRAY_BUFFER, scene.relief.idx);
    gl.drawElements(gl.TRIANGLES, scene.relief.n, gl.UNSIGNED_INT, 0);

    gl.useProgram(progMarques);
    gl.uniformMatrix4fv(gl.getUniformLocation(progMarques, "uMVP"), false, mvp);
    gl.uniform1f(gl.getUniformLocation(progMarques, "uExag"), exag);
    gl.uniform1f(gl.getUniformLocation(progMarques, "uTaille"), 12 * dpr);
    gl.uniform1f(gl.getUniformLocation(progMarques, "uRond"), 0);
    lier(progMarques, "aPos", scene.tiges.pos, 3);
    lier(progMarques, "aCol", scene.tiges.col, 3);
    gl.drawArrays(gl.LINES, 0, scene.tiges.n);
    gl.uniform1f(gl.getUniformLocation(progMarques, "uRond"), 1);
    lier(progMarques, "aPos", scene.tetes.pos, 3);
    lier(progMarques, "aCol", scene.tetes.col, 3);
    gl.drawArrays(gl.POINTS, 0, scene.tetes.n);

    placerEtiquettes(mvp);
  }

  // --- Étiquettes HTML ----------------------------------------------------------
  let etiquettes = [];
  function construireEtiquettes() {
    const boite = $("etiquettes");
    boite.textContent = "";
    etiquettes = scene.points.slice(0, LIMITE_ETIQUETTES).map((p) => {
      const d = document.createElement("div");
      d.className = "etq";
      d.textContent = p.name + " ";
      const s = document.createElement("small");
      s.textContent = Math.round(p.altitude) + " m";
      d.appendChild(s);
      boite.appendChild(d);
      return { p, d };
    });
  }

  function placerEtiquettes(mvp) {
    const visibles = $("noms").checked;
    const w = canvas.clientWidth, h = canvas.clientHeight;
    for (const { p, d } of etiquettes) {
      // Même transformation que le shader : toute la verticale est multipliée.
      const y = (p.y + scene.hauteurTige) * exag;
      const v = [p.x, y, p.z, 1];
      const c = [0, 1, 2, 3].map((r) => mvp[r] * v[0] + mvp[4 + r] * v[1] + mvp[8 + r] * v[2] + mvp[12 + r]);
      const dedans = c[3] > 0 && Math.abs(c[0] / c[3]) < 1.1 && Math.abs(c[1] / c[3]) < 1.1;
      d.hidden = !visibles || !dedans;
      if (!d.hidden) {
        d.style.left = ((c[0] / c[3] + 1) / 2) * w + "px";
        d.style.top = ((1 - c[1] / c[3]) / 2) * h + "px";
      }
    }
  }

  // --- Interaction -------------------------------------------------------------
  let glisse = null;
  canvas.addEventListener("contextmenu", (e) => e.preventDefault());
  canvas.addEventListener("pointerdown", (e) => {
    glisse = { x: e.clientX, y: e.clientY, deplacer: e.button === 2 || e.shiftKey };
    canvas.setPointerCapture(e.pointerId);
  });
  canvas.addEventListener("pointerup", () => { glisse = null; });
  canvas.addEventListener("pointermove", (e) => {
    if (!glisse) return;
    const dx = e.clientX - glisse.x, dy = e.clientY - glisse.y;
    glisse.x = e.clientX; glisse.y = e.clientY;
    if (glisse.deplacer) {
      const k = (cam.dist / canvas.clientHeight) * SENSIBILITE_DEPLACEMENT;
      const cy = Math.cos(cam.yaw), sy = Math.sin(cam.yaw);
      // Avant-arrière (dy) : le terrain suit la souris, comme sur la carte 2D.
      cam.cible[0] -= (dx * cy + dy * sy) * k;
      cam.cible[2] -= (-dx * sy + dy * cy) * k;
    } else {
      cam.yaw -= dx * 0.006;
      cam.pitch = Math.min(1.5, Math.max(0.08, cam.pitch + dy * 0.006));
    }
    dessiner();
  });
  canvas.addEventListener("wheel", (e) => {
    e.preventDefault();
    cam.dist = Math.min(5000, Math.max(0.2, cam.dist * Math.exp(e.deltaY * 0.0012)));
    dessiner();
  }, { passive: false });
  window.addEventListener("resize", dessiner);
  $("exag").addEventListener("input", (e) => {
    exag = Number(e.target.value);
    $("exag-val").textContent = String(exag).replace(".", ",");
    dessiner();
  });
  $("noms").addEventListener("change", dessiner);
  $("fond").addEventListener("change", dessiner);
  $("recentrer").addEventListener("click", recentrer);

  /**
   * Libération explicite de la mémoire graphique à la fermeture du panneau :
   * sans cela, WebView2 garde tampons et texture jusqu'au ramasse-miettes.
   */
  function liberer() {
    if (scene) {
      for (const groupe of [scene.relief, scene.tiges, scene.tetes]) {
        for (const b of Object.values(groupe)) if (b instanceof WebGLBuffer) gl.deleteBuffer(b);
      }
      if (scene.topo) gl.deleteTexture(scene.topo);
      scene = null;
    }
    const perte = gl.getExtension("WEBGL_lose_context");
    if (perte) perte.loseContext();
  }
  window.addEventListener("pagehide", liberer);

  let dejaDemarre = false;
  async function demarrer(projectId) {
    if (dejaDemarre) return;
    dejaDemarre = true;
    if (!projectId) { statut("Ouvrez un projet pour afficher sa carte 3D."); return; }
    chargement(true);
    let data;
    try {
      data = await appeler("scene", { project_id: projectId });
    } finally {
      chargement(false);
    }
    construire(data);
    statut("");
    $("barre").hidden = false;
    $("source").textContent = `Relief : ${data.terrain.source || "IGN"}` +
      (data.texture ? " · Fond : Plan IGN v2" : " · fond topo indisponible") + " (data.geopf.fr)";
    if (data.points.length > LIMITE_ETIQUETTES) {
      $("aide").textContent += ` · noms affichés pour ${LIMITE_ETIQUETTES} points sur ${data.points.length}`;
    }
  }
})();
