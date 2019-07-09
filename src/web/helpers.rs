use handlebars::{Handlebars, Helper, RenderContext, RenderError, HelperResult, Output, Context};


pub fn helper_duration(h: &Helper, _: &Handlebars, _: &Context, _: &mut RenderContext,
                   out: &mut dyn Output) -> HelperResult {
    let secs = h.param(0).and_then(|v| v.value().as_u64())
        .ok_or(RenderError::new("int param not found"))?;
    let d = secs / 86400;
    let h = (secs % 86400) / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    vec![(d, 'd'), (h, 'h'), (m, 'm'), (s, 's')].into_iter()
        .filter(|(v, _)| *v > 0)
        .take(2)
        .try_for_each(|(v, u)| {
            out.write(&format!("{}{}", v, u))
        })?;
    Ok(())
}

