/* this is a NodeJS script.
 * it will ran by the `__00_format.cmd` if that batch file will find `node.exe` that is newer than v20.1 in the PATH.
 * you can download nodejs single binary from here:
 * nodejs.org/download/nightly/v27.0.0-nightly20261027640ce854c5/win-x64/node.exe
 *
 * this NodeJS script accepts either single file-path or single directory-path, 
 * as the first single argument.
 * - for single file it apply the modification and returns.
 * - for directory, it uses `readdir` with `recursive`, then excludes unsuitable entries modify each.
 *
 * files are being backed-up before modification. 
 * you have to manually remove the backups. the format is `.rs.__(timestamp).bak` for the extension.
 *
 * the purpose of this file is to break code by adding two new lines before specific segments,
 * this allows to `cargo fmt` to handle even minified code.
 *
 * the code skips non `.rs` files, 
 * skips files with a variation of jetbrains IDE no-formatting (even if it is for a small segment of the file), 
 * as well as rust no-formatting (normally at the head of the file).
 *
 * - if the pattern to adding newlines before is not found, the file won't be written with same content.
 * - if file's parent directories include some of known directories to be ignored, the file won't be processed.
 * - if any of the async methods in `file_manipulation` function would fail the final overwriting will fail,
 *   this is good, it means if writing backup (pre-overwrite) fails the file won't be modified.
 *
 * there are some error codes that simplifies top level errors such as invalid input, as well as zero success no modified files,  
 * those aren't strictly speaking "errors". any errors "per file" are not considered a "program-exit-worthy" since the Promise 
 * handles any error by simply let `Promise.allSettled` finish everything and simply report..
 * note the `settled_results` size matches `files` array and order.
 */

"use strict";

if (!process.argv[2]) {
  console.log("error: missing argument. provide a file or directory path (exit code: 1).");
  process.exitCode = 1;
  process.exit(1);
}

const EOL         = "\r\n" //require("os").EOL
     ,TIMESTAMP   = (new Date()).toISOString().replace(/[\-T\:]+/gm,"_").replace(/\..+$/gm,"")
     ,fs          = require("fs/promises")
     ,path        = require("path")
     ,arg         = path.resolve( process.argv[2] )
     ,allowed_ext = /\.(rs)$/i
     ,skip_no_fmt = [/\/\/\s*@formatter:off/
                    ,/#!\[allow\(rustfmt::skip\)\]/
                    ]
     ,skip_dir    = /[\/\\](\.git|node_modules|\.venv|target|\.idea|\.cargo|build|dist|\.next|out|__pycache__)[\/\\]/i
     ;

const file_manipulation = async (file)=>{
  const content = await fs.readFile(file, {"encoding":"utf8", signal:AbortSignal.timeout(10 * 1000)});

  if(skip_no_fmt[0].test(content) || skip_no_fmt[1].test(content)){
    throw new Error("file has an instruction to avoid formatting, as a safety measure I will skip modifying this file.");
  }


  let modified_content = content;
  modified_content = modified_content.replace(/\r+/gms,"")
                                     .replace(/}\n*(\s*)(#\[|pub|async|fn|struct|enum|trait|impl|mod|type|const|static)/gms, "}\n\n$1$2")
                                     .replace(/;\n*(\s*)(#\[|pub|async|fn|struct|impl|mod)/gms, ";\n\n$1$2")
                                     .replace(/\n/gms,EOL)
                                     ;

  if(content === modified_content){
    throw new Error("no change in content after replace.");
  }

  await fs.writeFile(file + ".__" + TIMESTAMP + ".bak", content, {"encoding":"utf8"}); /* backup original content before overwriting original file. */

  await fs.writeFile(file, modified_content, {"encoding":"utf8"});

  return file;  /*just to have path back.. */
};

(async ()=>{
  console.log("input: [" + arg + "]");

  console.log("locating suitable '.rs' files...");
  const arg_stat = await fs.lstat(arg, {signal:AbortSignal.timeout(10000)});
  if(arg_stat.isSymbolicLink() || (!arg_stat.isFile() && !arg_stat.isDirectory()) ){
    console.log("error: target is either a symlink, or not a file and not a directory (exit code:2).");
    process.exitCode = 2; 
    process.exit(2);
  }

  let files = [];
  if(true === arg_stat.isFile()){
    console.log("input: [" + arg + "] - is a file.");
    files = [arg];
  }else{
    console.log("input: [" + arg + "] - is a directory.");
    files = (await fs.readdir(arg, {withFileTypes:true, recursive:true}));
    console.log("total number of files and directories: [" + files.length + "]");
    files = files.filter(entry=>(true  === entry.isFile()) );
    console.log("after filtering out non-files entries: [" + files.length + "]");
    files = files.filter(entry=>(false === entry.isSymbolicLink()) );
    console.log("after filtering out symlink files: [" + files.length + "]");
    files = files.filter(entry=>(false === skip_dir.test( "/" + entry.parentPath + "/" )) ); /* string adding here is just to normalize cases for the regexp since end directories won't have ending '/' */
    console.log("after filtering out files from directories that should be ignored: [" + files.length + "]");
    files = files.filter(entry=>(true  === allowed_ext.test(entry.name)) );
    console.log("after filtering out files with unsuitable extensions: [" + files.length + "]");

    files = files.map(entry=>(entry.parentPath + "/" + entry.name));  /* flatten 'entry' to string */
  }

  if(0 === files.length){
    console.log("no files found to process (exit code: 3)."); 
    process.exitCode = 3; 
    process.exit(3);
  }

  files = files.map(file=>file.replace(/[\/\\]+/gm,"/"))
               .sort()
               ;

  console.log("processing [" + files.length + "] files...");

  const settled_results = await Promise.allSettled(files.map(file=>file_manipulation(file))); /* store results for console-log statistics */
  settled_results.forEach((settled_result,index)=>{
    const total = String(settled_results.length);
    const index_str = String(index).padStart(total.length, "0");  /* number to string, now .length counts characters, used for zero-padding index value. */
    console.log(
      index_str + "/" + total + ".  "
     + "[" + ("fulfilled" === settled_result.status ? "✓" : "✗") + "]  "
     + "[" + files[index] + "]  "
     + (settled_result.reason && settled_result.reason.message ? ("(error: " + settled_result.reason.message + ")") : "")
    );
  });


  const num_fulfilled   = settled_results.reduce((carry, current)=>{carry += ("fulfilled" === current.status ? 1 : 0); return carry;},0); /* count success. */

  console.log("modified [" + num_fulfilled + "/" + settled_results.length + "] files"
            + "(" + ((num_fulfilled / settled_results.length)*100).toFixed(2) + "% success rate)."
             );
 
  if(0 === num_fulfilled){
    console.log("zero success, no files modified. (exit code: 4)."); 
    process.exitCode = 4; 
    process.exit(4);
  }

})();
