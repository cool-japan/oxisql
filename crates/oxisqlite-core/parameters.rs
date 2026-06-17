use std::num::NonZeroUsize;

pub const PARAM_PREFIX: &str = "__param_";

#[derive(Clone, Debug)]
pub enum Parameter {
    Anonymous(NonZeroUsize),
    Indexed(NonZeroUsize),
    Named(String, NonZeroUsize),
}

impl PartialEq for Parameter {
    fn eq(&self, other: &Self) -> bool {
        self.index() == other.index()
    }
}

impl Parameter {
    pub fn index(&self) -> NonZeroUsize {
        match self {
            Parameter::Anonymous(index) => *index,
            Parameter::Indexed(index) => *index,
            Parameter::Named(_, index) => *index,
        }
    }
}

#[derive(Debug)]
pub struct Parameters {
    index: NonZeroUsize,
    pub list: Vec<Parameter>,
}

impl Default for Parameters {
    fn default() -> Self {
        Self::new()
    }
}

impl Parameters {
    pub fn new() -> Self {
        // SAFETY: 1 is non-zero
        Self {
            index: unsafe { NonZeroUsize::new_unchecked(1) },
            list: vec![],
        }
    }

    pub fn count(&self) -> usize {
        let mut params = self.list.clone();
        params.dedup();
        params.len()
    }

    pub fn name(&self, index: NonZeroUsize) -> Option<String> {
        self.list.iter().find_map(|p| match p {
            Parameter::Anonymous(i) if *i == index => Some("?".to_string()),
            Parameter::Indexed(i) if *i == index => Some(format!("?{i}")),
            Parameter::Named(name, i) if *i == index => Some(name.to_owned()),
            _ => None,
        })
    }

    pub fn index(&self, name: impl AsRef<str>) -> Option<NonZeroUsize> {
        self.list
            .iter()
            .find_map(|p| match p {
                Parameter::Named(n, index) if n == name.as_ref() => Some(index),
                _ => None,
            })
            .copied()
    }

    pub fn next_index(&mut self) -> crate::Result<NonZeroUsize> {
        let index = self.index;
        self.index = NonZeroUsize::new(self.index.get().saturating_add(1)).ok_or_else(|| {
            crate::LimboError::InternalError("parameter index overflow".to_string())
        })?;
        Ok(index)
    }

    pub fn push(&mut self, name: impl AsRef<str>) -> crate::Result<NonZeroUsize> {
        match name.as_ref() {
            param if param.is_empty() || param.starts_with(PARAM_PREFIX) => {
                let index = self.next_index()?;
                let use_idx = if let Some(idx) = param.strip_prefix(PARAM_PREFIX) {
                    idx.parse::<NonZeroUsize>().map_err(|e| {
                        crate::LimboError::InternalError(format!(
                            "failed to parse parameter index '{}': {}",
                            idx, e
                        ))
                    })?
                } else {
                    index
                };
                self.list.push(Parameter::Anonymous(use_idx));
                tracing::trace!("anonymous parameter at {use_idx}");
                Ok(use_idx)
            }
            name if name.starts_with(['$', ':', '@', '#']) => {
                match self
                    .list
                    .iter()
                    .find(|p| matches!(p, Parameter::Named(n, _) if name == n))
                {
                    Some(t) => {
                        let index = t.index();
                        self.list.push(t.clone());
                        tracing::trace!("named parameter at {index} as {name}");
                        Ok(index)
                    }
                    None => {
                        let index = self.next_index()?;
                        self.list.push(Parameter::Named(name.to_owned(), index));
                        tracing::trace!("named parameter at {index} as {name}");
                        Ok(index)
                    }
                }
            }
            index => {
                let index: NonZeroUsize = index.parse().map_err(|e| {
                    crate::LimboError::InternalError(format!(
                        "failed to parse parameter index '{}': {}",
                        index, e
                    ))
                })?;
                if index > self.index {
                    self.index =
                        NonZeroUsize::new(index.get().saturating_add(1)).ok_or_else(|| {
                            crate::LimboError::InternalError("parameter index overflow".to_string())
                        })?;
                }
                self.list.push(Parameter::Indexed(index));
                tracing::trace!("indexed parameter at {index}");
                Ok(index)
            }
        }
    }
}
