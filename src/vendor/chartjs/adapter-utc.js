/*
 * Adaptateur de dates pour l'échelle `time` de Chart.js — **UTC strict**.
 *
 * ## Pourquoi ce fichier existe
 *
 * Chart.js ne sait pas manipuler les dates tout seul : son échelle `time`
 * délègue à un adaptateur, et les adaptateurs officiels embarquent une
 * bibliothèque de dates complète (date-fns, Luxon ou Moment). Vendoriser l'une
 * d'elles ajouterait 40 à 200 Ko et une seconde dépendance à suivre, pour un
 * usage qui tient dans les méthodes `Date.prototype.*UTC*` du navigateur.
 *
 * ## Pourquoi UTC, et pas l'heure locale
 *
 * Ce n'est pas une simplification, c'est la règle n°1 du projet (README § 3.3) :
 * tout instant venant du backend est de l'UTC absolu, et l'application l'affiche
 * en UTC en le disant. Un adaptateur en heure locale décalerait silencieusement
 * l'axe du graphique de une ou deux heures selon la saison, et un minimum
 * nocturne relevé à 05 h UTC apparaîtrait à 07 h en été — exactement le genre
 * d'ambiguïté que le stockage en UTC absolu a été mis en place pour éliminer.
 *
 * Toutes les méthodes ci-dessous lisent et écrivent donc en UTC, sans exception.
 */

(function () {
  "use strict";

  if (typeof Chart === "undefined") {
    // Chargé avant Chart.js : rien à greffer. L'ordre des <script> est fautif.
    console.error("[gws] adapter-utc.js chargé avant chart.umd.js");
    return;
  }

  const MS = {
    millisecond: 1,
    second: 1000,
    minute: 60000,
    hour: 3600000,
    day: 86400000,
    week: 604800000,
  };

  const MOIS_COURTS = [
    "janv.",
    "févr.",
    "mars",
    "avr.",
    "mai",
    "juin",
    "juil.",
    "août",
    "sept.",
    "oct.",
    "nov.",
    "déc.",
  ];

  const pad = (n, w = 2) => String(n).padStart(w, "0");

  /** Découpe un instant en ses composantes UTC. */
  function parts(ts) {
    const d = new Date(ts);
    return {
      y: d.getUTCFullYear(),
      mo: d.getUTCMonth(),
      da: d.getUTCDate(),
      h: d.getUTCHours(),
      mi: d.getUTCMinutes(),
      s: d.getUTCSeconds(),
      ms: d.getUTCMilliseconds(),
      dow: d.getUTCDay(), // 0 = dimanche
    };
  }

  const utc = (y, mo, da, h, mi, s, ms) => Date.UTC(y, mo, da || 1, h || 0, mi || 0, s || 0, ms || 0);

  Chart._adapters._date.override({
    _id: "gws-utc",

    /**
     * Formats par unité d'axe. Chart.js choisit l'unité, ces clés nomment le
     * rendu correspondant — les valeurs sont des étiquettes internes lues par
     * `format()` ci-dessous, pas des motifs à interpréter.
     */
    formats() {
      return {
        millisecond: "hms",
        second: "hms",
        minute: "hm",
        hour: "hm",
        day: "jour",
        week: "jour",
        month: "mois",
        quarter: "mois",
        year: "annee",
      };
    },

    parse(value) {
      if (value === null || value === undefined) return null;
      if (value instanceof Date) return value.getTime();
      if (typeof value === "number") return isFinite(value) ? value : null;
      const t = Date.parse(value);
      return isNaN(t) ? null : t;
    },

    format(ts, fmt) {
      const p = parts(ts);
      switch (fmt) {
        case "hms":
          return `${pad(p.h)}:${pad(p.mi)}:${pad(p.s)}`;
        case "hm":
          return `${pad(p.h)}:${pad(p.mi)}`;
        case "jour":
          return `${pad(p.da)} ${MOIS_COURTS[p.mo]}`;
        case "mois":
          return `${MOIS_COURTS[p.mo]} ${p.y}`;
        case "annee":
          return String(p.y);
        // Format complet : utilisé par l'infobulle, où la date entière compte.
        default:
          return `${pad(p.da)}/${pad(p.mo + 1)}/${p.y} ${pad(p.h)}:${pad(p.mi)} UTC`;
      }
    },

    add(ts, amount, unit) {
      const p = parts(ts);
      switch (unit) {
        case "quarter":
          return utc(p.y, p.mo + amount * 3, p.da, p.h, p.mi, p.s, p.ms);
        case "month":
          return utc(p.y, p.mo + amount, p.da, p.h, p.mi, p.s, p.ms);
        case "year":
          return utc(p.y + amount, p.mo, p.da, p.h, p.mi, p.s, p.ms);
        default:
          return ts + amount * (MS[unit] || 0);
      }
    },

    diff(max, min, unit) {
      switch (unit) {
        case "month":
        case "quarter":
        case "year": {
          const a = parts(max);
          const b = parts(min);
          const mois = (a.y - b.y) * 12 + (a.mo - b.mo);
          if (unit === "year") return Math.trunc(mois / 12);
          if (unit === "quarter") return Math.trunc(mois / 3);
          return mois;
        }
        default:
          return Math.trunc((max - min) / (MS[unit] || 1));
      }
    },

    /**
     * `weekday` : jour de début de semaine demandé par Chart.js (0 = dimanche).
     * Par défaut on démarre le **lundi**, pour coïncider avec les seaux
     * hebdomadaires produits par `stats::Granularity::bucket_start` côté Rust.
     * Deux conventions différentes décaleraient l'axe des points d'un jour.
     */
    startOf(ts, unit, weekday) {
      const p = parts(ts);
      switch (unit) {
        case "second":
          return utc(p.y, p.mo, p.da, p.h, p.mi, p.s, 0);
        case "minute":
          return utc(p.y, p.mo, p.da, p.h, p.mi, 0, 0);
        case "hour":
          return utc(p.y, p.mo, p.da, p.h, 0, 0, 0);
        case "day":
          return utc(p.y, p.mo, p.da, 0, 0, 0, 0);
        case "week": {
          const debut = typeof weekday === "number" ? weekday : 1;
          const recul = (p.dow - debut + 7) % 7;
          return utc(p.y, p.mo, p.da - recul, 0, 0, 0, 0);
        }
        case "month":
          return utc(p.y, p.mo, 1, 0, 0, 0, 0);
        case "quarter":
          return utc(p.y, Math.floor(p.mo / 3) * 3, 1, 0, 0, 0, 0);
        case "year":
          return utc(p.y, 0, 1, 0, 0, 0, 0);
        case "isoWeek": {
          const recul = (p.dow + 6) % 7; // lundi
          return utc(p.y, p.mo, p.da - recul, 0, 0, 0, 0);
        }
        default:
          return ts;
      }
    },

    endOf(ts, unit) {
      // Dernière milliseconde de l'unité : début de la suivante, moins 1.
      return this.add(this.startOf(ts, unit), 1, unit === "isoWeek" ? "week" : unit) - 1;
    },
  });
})();
