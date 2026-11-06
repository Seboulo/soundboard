// Vérifie l'interface sans l'appli : elle est chargée dans Chromium avec un
// faux Tauri, avec différents réglages gardés (normaux, abîmés, anciens) et
// un état qui arrive en retard. Une erreur au lancement bloquait tout
// (2.4.0 : onglet Envoyer coincé). Lancement : node tests/ui-smoke.js
const http = require('http');
const fs = require('fs');
const path = require('path');
const { chromium } = require('playwright');
const vs = JSON.stringify({q:'chat',results:[{url:'https://www.youtube.com/watch?v=dQw4w9WgXcQ',title:'t',thumb:'x',duration_s:200}],gifs:[{anim:'a',big:'b',title:'g'}]});
const scenarios = {
  vide: {},
  realiste: { 'soundboard.clipSearch': vs, 'soundboard.tab': 'Memes', 'soundboard.recent': '["a"]', 'soundboard.plays': '{"a":3}', 'soundboard.closedGroups': '["Memes"]', 'soundboard.sort':'name', 'soundboard.view':'list', 'soundboard.theme':'dark', 'soundboard.padSize':'large', 'soundboard.display':'{"zoom":1.2}', 'soundboard.settingsTab':'friends', 'soundboard.source':'freesound', 'soundboard.tts':'{}', 'soundboard.onlineVolume':'0.5', 'soundboard.advanced':'1' },
  onglet_disparu: { 'soundboard.tab': 'OngletSupprime', 'soundboard.clipSearch': vs },
  json_casse: { 'soundboard.clipSearch': '{oops', 'soundboard.recent': 'nope', 'soundboard.plays': '[', 'soundboard.closedGroups': '{"x":1}', 'soundboard.display': 'xx', 'soundboard.tts': '1' },
  types_bizarres: { 'soundboard.clipSearch': JSON.stringify({q:'x',results:[null,1,'a'],gifs:'z'}), 'soundboard.recent': '[null,5]', 'soundboard.plays': 'null', 'soundboard.closedGroups': 'null', 'soundboard.display':'null','soundboard.tts':'null' },
  vieille_recherche: { 'soundboard.clipSearch': JSON.stringify({q:'chat',results:[{url:'u',title:'t',thumb:'x'}]}) },
};
const UI = path.join(__dirname, '..', 'ui');
const TYPES = { '.html': 'text/html', '.js': 'text/javascript', '.css': 'text/css', '.png': 'image/png', '.svg': 'image/svg+xml' };
(async () => {
  const server = http.createServer((req, res) => {
    const file = path.join(UI, decodeURIComponent(req.url.split('?')[0]));
    if (!file.startsWith(UI) || !fs.existsSync(file) || fs.statSync(file).isDirectory()) { res.writeHead(404); return res.end(); }
    res.writeHead(200, { 'Content-Type': TYPES[path.extname(file)] || 'application/octet-stream' });
    fs.createReadStream(file).pipe(res);
  }).listen(8765);
  let bad = 0;
  const b = await chromium.launch();
  for (const [name, ls] of Object.entries(scenarios)) {
    for (const slow of [false, true]) {
      const p = await b.newPage();
      const errs = []; p.on('pageerror', e => errs.push(e.stack.split('\n').slice(0,2).join(' | ')));
      await p.addInitScript(({ls, slow}) => {
        try { for (const [k,v] of Object.entries(ls)) localStorage.setItem(k, v); } catch {}
        const snap = { config: { sounds: [{id:'a',name:'Son',path:'x.mp3',volume:1,category:'Memes',favorite:false}], categories:['Memes'], settings: { room_code: 'ABCD', giphy_key: 'k', setup_done: true, room_blocked: [] } }, devices: { outputs: [], inputs: [] }, discord_heard: false, warnings: [] };
        const room = {on:true,connected:true,members:[{id:'p',name:'Pote'}],target:null};
        const delay = (ms) => new Promise(r => setTimeout(r, ms));
        window.__TAURI__ = { core: { invoke: async (c) => {
          if (c==='get_state' && slow) await delay(800);
          if (c==='room_status') return room; if (c==='clip_recents') return [];
          if (c==='clip_history') return {sent:[{path:'/e/envoi/a.mp4',kind:'video',title:'v',at:1}],received:[{path:'/e/recus/b.png',kind:'image',title:'',from:'Pote',at:2}]};
          if (c==='search_images'||c==='clip_search_videos') return [];
          return snap; }, convertFileSrc: (x)=>x }, event: { listen: async () => () => {} }, app: { getVersion: async () => '2.4.1' }, window: { getCurrentWindow: () => ({}) }, webviewWindow: {} };
      }, {ls, slow});
      await p.goto('http://localhost:8765/index.html'); await p.waitForTimeout(1500);
      const fail = [];
      const tryDo = async (label, f) => { try { await f(); } catch (e) { fail.push(label); } };
      await tryDo('onglet Envoyer', () => p.click('#tab-send', {timeout: 2000}));
      await tryDo('recherche', async () => { await p.fill('#dlg-clip [name=q]','chien',{timeout:2000}); await p.click('#dlg-clip [data-action="search"]',{timeout:2000}); });
      await tryDo('historique', () => p.click('#dlg-clip [data-source="history"]', {timeout: 2000}));
      await p.waitForTimeout(300);
      await tryDo('clic historique', () => p.click('#dlg-clip [data-list="received"] button', {timeout: 2000}));
      await tryDo('retour aux sons', () => p.click('button.tab:not(#tab-send)', {timeout: 2000}));
      await p.waitForTimeout(300);
      const grid = await p.isVisible('#grid');
      if (!grid || fail.length || errs.length) bad++;
      console.log(`${name}${slow?' (lent)':''}: grille=${grid} échecs=${JSON.stringify(fail)} erreurs=${JSON.stringify(errs)}`);
      await p.close();
    }
  }
  await b.close();
  server.close();
  if (bad) { console.error(`${bad} cas en échec`); process.exit(1); }
  console.log('Interface OK');
})();
