//! 結果からのドラッグ＆ドロップ（OLE DoDragDrop、CF_HDROP）。
//!
//! UI スレッドで呼ぶ。`DoDragDrop` はモーダルループなので、ドラッグ中は egui の描画が止まる
//! （Explorer から Explorer へドラッグするときと同じ）。

use windows::core::{implement, Result, HRESULT};
use windows::Win32::Foundation::{DRAGDROP_S_CANCEL, DRAGDROP_S_DROP, DRAGDROP_S_USEDEFAULTCURSORS, DV_E_FORMATETC, E_NOTIMPL, OLE_E_ADVISENOTSUPPORTED, S_OK};
use windows::Win32::System::Com::{IAdviseSink, IDataObject, IDataObject_Impl, IEnumFORMATETC, IEnumSTATDATA, DATADIR_GET, DVASPECT_CONTENT, FORMATETC, STGMEDIUM, STGMEDIUM_0, TYMED_HGLOBAL};
use windows::Win32::Foundation::HGLOBAL;
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock, GMEM_MOVEABLE};
use windows::Win32::System::Ole::{DoDragDrop, IDropSource, IDropSource_Impl, OleInitialize, DROPEFFECT, DROPEFFECT_COPY, DROPEFFECT_LINK, DROPEFFECT_MOVE};
use windows::Win32::System::SystemServices::{MODIFIERKEYS_FLAGS, MK_LBUTTON};
use windows::Win32::UI::Shell::{SHCreateStdEnumFmtEtc, DROPFILES};

const CF_HDROP: u16 = 15;

/// CF_HDROP 用の HGLOBAL（DROPFILES + 二重 NUL 終端のワイド文字列）を作る。
unsafe fn make_hdrop(paths: &[String]) -> Result<HGLOBAL> {
    let mut list: Vec<u16> = Vec::new();
    for p in paths {
        list.extend(p.encode_utf16());
        list.push(0);
    }
    list.push(0);
    let header = std::mem::size_of::<DROPFILES>();
    let total = header + list.len() * 2;
    let h = GlobalAlloc(GMEM_MOVEABLE, total)?;
    let p = GlobalLock(h) as *mut u8;
    let df = p as *mut DROPFILES;
    (*df).pFiles = header as u32;
    (*df).pt.x = 0;
    (*df).pt.y = 0;
    (*df).fNC = false.into();
    (*df).fWide = true.into();
    std::ptr::copy_nonoverlapping(list.as_ptr() as *const u8, p.add(header), list.len() * 2);
    let _ = GlobalUnlock(h);
    Ok(h)
}

unsafe fn dup_hglobal(src: HGLOBAL) -> Result<HGLOBAL> {
    let n = GlobalSize(src);
    let dst = GlobalAlloc(GMEM_MOVEABLE, n)?;
    let s = GlobalLock(src) as *const u8;
    let d = GlobalLock(dst) as *mut u8;
    std::ptr::copy_nonoverlapping(s, d, n);
    let _ = GlobalUnlock(src);
    let _ = GlobalUnlock(dst);
    Ok(dst)
}

#[implement(IDataObject)]
struct FileData {
    hdrop: HGLOBAL,
}

fn is_hdrop(f: &FORMATETC) -> bool {
    f.cfFormat == CF_HDROP && (f.tymed & TYMED_HGLOBAL.0 as u32) != 0 && f.dwAspect == DVASPECT_CONTENT.0
}

impl IDataObject_Impl for FileData_Impl {
    fn GetData(&self, pformatetcin: *const FORMATETC) -> Result<STGMEDIUM> {
        // SAFETY: 呼び出し側が有効な FORMATETC を渡す。
        let f = unsafe { &*pformatetcin };
        if !is_hdrop(f) {
            return Err(DV_E_FORMATETC.into());
        }
        let h = unsafe { dup_hglobal(self.hdrop)? };
        Ok(STGMEDIUM { tymed: TYMED_HGLOBAL.0 as u32, u: STGMEDIUM_0 { hGlobal: h }, pUnkForRelease: std::mem::ManuallyDrop::new(None) })
    }
    fn GetDataHere(&self, _pformatetc: *const FORMATETC, _pmedium: *mut STGMEDIUM) -> Result<()> {
        Err(E_NOTIMPL.into())
    }
    fn QueryGetData(&self, pformatetc: *const FORMATETC) -> HRESULT {
        let f = unsafe { &*pformatetc };
        if is_hdrop(f) {
            S_OK
        } else {
            DV_E_FORMATETC
        }
    }
    fn GetCanonicalFormatEtc(&self, _pformatectin: *const FORMATETC, pformatetcout: *mut FORMATETC) -> HRESULT {
        unsafe {
            (*pformatetcout).ptd = std::ptr::null_mut();
        }
        E_NOTIMPL
    }
    fn SetData(&self, _pformatetc: *const FORMATETC, _pmedium: *const STGMEDIUM, _frelease: windows::core::BOOL) -> Result<()> {
        Err(E_NOTIMPL.into())
    }
    fn EnumFormatEtc(&self, dwdirection: u32) -> Result<IEnumFORMATETC> {
        if dwdirection != DATADIR_GET.0 as u32 {
            return Err(E_NOTIMPL.into());
        }
        let f = FORMATETC { cfFormat: CF_HDROP, ptd: std::ptr::null_mut(), dwAspect: DVASPECT_CONTENT.0, lindex: -1, tymed: TYMED_HGLOBAL.0 as u32 };
        unsafe { SHCreateStdEnumFmtEtc(&[f]) }
    }
    fn DAdvise(&self, _pformatetc: *const FORMATETC, _advf: u32, _padvsink: windows::core::Ref<'_, IAdviseSink>) -> Result<u32> {
        Err(OLE_E_ADVISENOTSUPPORTED.into())
    }
    fn DUnadvise(&self, _dwconnection: u32) -> Result<()> {
        Err(OLE_E_ADVISENOTSUPPORTED.into())
    }
    fn EnumDAdvise(&self) -> Result<IEnumSTATDATA> {
        Err(OLE_E_ADVISENOTSUPPORTED.into())
    }
}

#[implement(IDropSource)]
struct Source;

impl IDropSource_Impl for Source_Impl {
    fn QueryContinueDrag(&self, fescapepressed: windows::core::BOOL, grfkeystate: MODIFIERKEYS_FLAGS) -> HRESULT {
        if fescapepressed.as_bool() {
            return DRAGDROP_S_CANCEL;
        }
        if (grfkeystate.0 & MK_LBUTTON.0) == 0 {
            return DRAGDROP_S_DROP;
        }
        S_OK
    }
    fn GiveFeedback(&self, _dweffect: DROPEFFECT) -> HRESULT {
        DRAGDROP_S_USEDEFAULTCURSORS
    }
}

/// 検証用: データオブジェクトを作り、CF_HDROP を取り出してパス列を復元する。
pub fn probe(paths: &[String]) -> Result<Vec<String>> {
    use windows::Win32::System::Com::{DVASPECT_CONTENT, FORMATETC, TYMED_HGLOBAL};
    unsafe {
        let _ = OleInitialize(None);
        let hdrop = make_hdrop(paths)?;
        let data: IDataObject = FileData { hdrop }.into();
        let f = FORMATETC { cfFormat: CF_HDROP, ptd: std::ptr::null_mut(), dwAspect: DVASPECT_CONTENT.0, lindex: -1, tymed: TYMED_HGLOBAL.0 as u32 };
        data.QueryGetData(&f).ok()?;
        let med = data.GetData(&f)?;
        let h = med.u.hGlobal;
        let n = GlobalSize(h);
        let p = GlobalLock(h) as *const u8;
        let bytes = std::slice::from_raw_parts(p, n);
        let header = std::mem::size_of::<DROPFILES>();
        let units: Vec<u16> = bytes[header..].as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        let mut out = Vec::new();
        for part in units.split(|&u| u == 0) {
            if part.is_empty() {
                break;
            }
            out.push(String::from_utf16_lossy(part));
        }
        let _ = GlobalUnlock(h);
        Ok(out)
    }
}

/// ドラッグを開始する。ドロップ先が受け取るまで（またはキャンセルまで）ブロックする。
pub fn start_drag(paths: &[String]) -> Result<()> {
    if paths.is_empty() {
        return Ok(());
    }
    // SAFETY: OLE 初期化は UI スレッドで一度でよい（多重呼び出しは S_FALSE で無害）。
    unsafe {
        let _ = OleInitialize(None);
        let hdrop = make_hdrop(paths)?;
        let data: IDataObject = FileData { hdrop }.into();
        let src: IDropSource = Source.into();
        let mut effect = DROPEFFECT(0);
        let _ = DoDragDrop(&data, &src, DROPEFFECT_COPY | DROPEFFECT_MOVE | DROPEFFECT_LINK, &mut effect);
    }
    Ok(())
}
