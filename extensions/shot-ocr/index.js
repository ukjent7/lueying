// 掠影官方扩展:截图 → 识别 → 上屏。
// 只用公开接口(capture / ocr / output / notify),不读候选文字,不联网。
lueying.command("run", async function () {
  var shot = await lueying.capture.shot({});
  var ocr = await lueying.ocr.recognize({ file: shot.file });
  var text = String(ocr.text || "").trim();
  if (!text) {
    await lueying.notify.show({ title: "截图 OCR", body: "没有识别到文字" });
    return { chars: 0 };
  }
  await lueying.output.deliver({ text: text });
  return { chars: text.length, lang: ocr.lang };
});
