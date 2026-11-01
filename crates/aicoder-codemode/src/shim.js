"use strict";
{
  const catalog = JSON.parse(__catalog);
  const metadata = catalog.map(t => Object.freeze({name:t.name, alias:t.alias, description:t.description || ""}));
  Object.defineProperty(globalThis, "ALL_TOOLS", {value:Object.freeze(metadata), writable:false, configurable:false});
  globalThis.describeTool = async name => {
    const tool=catalog.find(t=>t.name===name);
    return tool ? {name:tool.name,alias:metadata.find(t=>t.name===name).alias,description:tool.description || "",parameters:tool.parameters} : undefined;
  };
  globalThis.describeNamespace = async name => {
    const tools=metadata.filter(t=>t.name.startsWith(name+"__"));
    return tools.length ? {name,tools} : undefined;
  };
  globalThis.searchTools = async (query, options={}) => {
    if (typeof query !== "string" || query.length>2048) throw new Error("Invalid tool search");
    const words=query.toLowerCase().split(/[^\p{L}\p{N}_]+/u).filter(Boolean).slice(0,32);
    const documents=metadata.filter(t=>!options.namespace || t.name.startsWith(options.namespace+"__"));
    const tokens=documents.map(t=>(t.name+" "+t.description).toLowerCase().split(/[^\p{L}\p{N}_]+/u));
    const average=tokens.reduce((n,t)=>n+t.length,0)/Math.max(1,tokens.length);
    return documents.map((tool,index)=>{
      const doc=tokens[index]; let score=0;
      for (const word of words) {
        const frequency=doc.filter(token=>token.includes(word)).length;
        const count=tokens.filter(d=>d.some(token=>token.includes(word))).length;
        if (frequency) score+=Math.log(1+(documents.length-count+.5)/(count+.5))*frequency*2.2/(frequency+1.2*(.25+.75*doc.length/Math.max(1,average)));
      }
      return {tool,score};
    }).filter(t=>t.score>0).sort((a,b)=>b.score-a.score || a.tool.name.localeCompare(b.tool.name)).slice(0,Math.max(1,Math.min(32,options.limit || 8))).map(t=>t.tool);
  };
  delete globalThis.__catalog;
  Object.freeze(tools);
  const log=(...values)=>__console(values);
  globalThis.console=Object.freeze({log,info:log,warn:log,error:log,debug:log});
  // No module/file/network/timer APIs are installed by this embedding.
}
