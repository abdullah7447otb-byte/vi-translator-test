const fs=require('fs');global.I18N={dicts:{},add(l,d){this.dicts[l]=d}};
eval(fs.readFileSync('i18n/ar.js','utf8'));
const ar=I18N.dicts.ar;
const src=fs.readFileSync('app.js','utf8')+fs.readFileSync('common.js','utf8')+fs.readFileSync('snapshot.js','utf8');
const re=/\bt\(\s*("(?:[^"\\]|\\.)*"|`[^`$]*`|'(?:[^'\\]|\\.)*')/g;let m;const miss=new Set();
while((m=re.exec(src))){let s;try{s=m[1][0]==='`'?m[1].slice(1,-1):JSON.parse(m[1][0]==="'"?'"'+m[1].slice(1,-1).replace(/"/g,'\\"')+'"':m[1]);}catch{continue}
 if(!(s in ar)) miss.add(s);}
// index.html data-i18n
const html=fs.readFileSync('index.html','utf8');
for(const mm of html.matchAll(/data-i18n[^>]*>([^<]+)</g)){const s=mm[1].trim(); if(s&&!(s in ar)) miss.add(s);}
for(const mm of html.matchAll(/data-i18n-title[^>]*title="([^"]+)"/g)){if(!(mm[1] in ar)) miss.add(mm[1]);}
console.log(miss.size);fs.writeFileSync('/tmp/shot/missing.json',JSON.stringify([...miss],null,1));
