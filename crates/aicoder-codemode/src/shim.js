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

// Image output is an ordinary scoped host-tool intent, not VM filesystem/network IO.
globalThis.image = async value => {
  if (Array.isArray(value)) { for (const item of value) await image(item); return; }
  if (value && Array.isArray(value._image_artifacts)) {
    for (const ref of value._image_artifacts) await __image({artifact_id:ref.id});
    return;
  }
  if (value && value.artifact && typeof value.artifact.id === "string") {
    await __image({artifact_id:value.artifact.id}); return;
  }
  if (value && typeof value.id === "string" && typeof value.media_type === "string") {
    await __image({artifact_id:value.id}); return;
  }
  if (value && typeof value.artifact_id === "string") {
    await __image({artifact_id:value.artifact_id}); return;
  }
  if (value && typeof value.data === "string") {
    await __image({data:value.data,media_type:value.mimeType || value.media_type}); return;
  }
  throw new TypeError("image() expects an image source or a tool .result() envelope with image artifacts");
};

// Host catalog data selects identities only; scripts never supply routes or credentials.
globalThis.models = Object.freeze({
  async getModelsOfType(type, provider) {
    if (typeof tools.model_catalog !== 'function') return [];
    return await tools.model_catalog({type,...(provider===undefined?{}:{provider})});
  },
  async getAvailableOfType(type, provider) {
    if (typeof tools.model_catalog !== 'function') return [];
    return await tools.model_catalog({type,available:true,...(provider===undefined?{}:{provider})});
  },
  async getModelOfType(type, provider, id) {
    return (await models.getModelsOfType(type,provider)).find(model=>model.id===id);
  },
  async generateImages(model, context) {
    const identity={provider:model.provider,id:model.id};
    try {
      if (typeof tools.model_generate_images !== 'function') throw new Error('No host image task tool is available');
      const input=context.input.map(block=>{
        if(block.type==='text') return {type:'text',text:block.text};
        if(block.type!=='image') throw new TypeError('Invalid image generation input');
        if(block.artifact) return {type:'image',source:{type:'artifact',reference:block.artifact}};
        if(block.source) return {type:'image',source:block.source};
        return {type:'image',source:{type:'base64',data:block.data,media_type:block.mimeType || block.media_type}};
      });
      return await tools.model_generate_images({model:identity,context:{input}});
    } catch(error) {
      const result={provider:identity.provider,model:identity.id,output:[],stopReason:'error',errorMessage:typeof error.message==='string'?error.message:'Image task failed'};
      const usage=error.details?.error?.details?.usage;
      if(usage!==undefined) result.usage=usage;
      return result;
    }
  },
  async classify(model, context) {
    const identity={provider:model.provider,id:model.id};
    try {
      if (typeof tools.model_classify !== 'function') throw new Error('No host classifier task tool is available');
      const request={state:context.state,questions:context.questions};
      if(context.images) request.images=context.images.flatMap(value=>{
        if(value && Array.isArray(value._image_artifacts)) return value._image_artifacts.map(reference=>({type:'artifact',reference}));
        if(value && value.artifact) return [{type:'artifact',reference:value.artifact}];
        if(value && value.type==='artifact') return [value];
        if(value && typeof value.data==='string') return [{type:'base64',data:value.data,media_type:value.mimeType || value.media_type}];
        if(value && typeof value.id==='string') return [{type:'artifact',reference:value}];
        throw new TypeError('Invalid classifier image source');
      });
      return await tools.model_classify({model:identity,context:request});
    } catch(error) {
      const result={provider:identity.provider,model:identity.id,answers:{},stopReason:'error',errorMessage:typeof error.message==='string'?error.message:'Classifier task failed'};
      const usage=error.details?.error?.details?.usage;
      if (usage!==undefined) result.usage=usage;
      return result;
    }
  }
});
