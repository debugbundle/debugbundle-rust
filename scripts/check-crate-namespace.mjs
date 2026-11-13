// Read-only public registry evidence. Absence never reserves a crate name.
import { mkdir,writeFile } from "node:fs/promises";
const output = new URL("../.tmp/registry/",import.meta.url);
await mkdir(output,{recursive:true});
const headers = {"user-agent":"debugbundle-rust-qualification/1.0 (https://github.com/debugbundle/debugbundle-rust)"};
const sources = [
  ["index","https://index.crates.io/de/bu/debugbundle"],
  ["crate","https://crates.io/api/v1/crates/debugbundle"],
  ["owners","https://crates.io/api/v1/crates/debugbundle/owners"]
];
const receipts=[];
for(const [name,url] of sources) {
  try {
    const response=await fetch(url,{headers,redirect:"error",signal:AbortSignal.timeout(15000)});
    let bytes=0;
    const chunks=[];
    for await(const chunk of response.body) {
      bytes+=chunk.byteLength;
      if(bytes>4*1024*1024)throw new Error("registry response too large");
      chunks.push(Buffer.from(chunk));
    }
    const body=Buffer.concat(chunks).toString("utf8");
    await writeFile(new URL(`${name}.txt`,output),body);
    receipts.push({source:name,url,status:response.status,bytes});
  } catch(error) {
    receipts.push({source:name,url,error:error.message});
  }
}
await writeFile(new URL("namespace.json",output),JSON.stringify({checked_at:new Date().toISOString(),name:"debugbundle",receipts,reservation:false},null,2));
for(const receipt of receipts)console.log(`${receipt.source}: ${receipt.status??receipt.error}`);
