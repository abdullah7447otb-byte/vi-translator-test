(function(){
const L={};
const PS5='PS5 — Barqawi';
const R={
 get_config:()=>structuredClone(window.MOCK_CFG),
 list_sources:()=>({windows:[{title:'PS Remote Play',process:'RemotePlay'}],devices:[],obs:{ok:false,error:'OBS is not running',sources:[]},screens:[],xbox:null,ps5:PS5}),
 ps5_status:()=>({signed_in:true,account_id:'AAAAAAAAAAA=',console:{name:'PS5',host:'192.168.1.20'},resolution:1080,high_bitrate:false,sound:true,volume:100,smooth:true,pin_wanted:null}),
 list_langs:()=>[{code:'ar',name:'Arabic',native:'العربية',rtl:true,google:'ar',azure:'ar',deepl:'AR',libre:'ar',local:'Xenova/opus-mt-en-ar',options:[]}],
 list_fonts:()=>['Noto Sans Arabic Medium'],
 has_offline:()=>true, dev_build:()=>false, app_version:()=>'1.4.4-test', update_status:()=>null,
 memory_status:()=>({entries:1240,hits:85,misses:32,avg_hit_ms:4,avg_miss_ms:110}),
 perf_status:()=>({read:{avg:38},translate:{avg:105},ready:{avg:160,max:320},minutes:12}),
 lan_info:()=>({url:'http://192.168.1.15:8791'}), list_tvs:()=>[], list_screens:()=>[],
 get_screen:()=>({layout:null,reason:[]}), frame_saver_status:()=>({on:false}),
 page_options:()=>({pages:[],options:[]}), sound_status:()=>({}), vcam_status:()=>({}), sound_inputs:()=>[],
 list_windows:()=>[],
};
window.__TAURI__={core:{invoke:async(n,a)=>{const f=R[n];return f?f(a):null;}},
 event:{listen:async(n,cb)=>{(L[n]=L[n]||[]).push(cb);return()=>{};}},
 window:{getCurrentWindow:()=>({setFullscreen:async()=>{}})}};
window.MOCK_EMIT=(n,p)=>(L[n]||[]).forEach(cb=>cb({payload:p}));
})();
