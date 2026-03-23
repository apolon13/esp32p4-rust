#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ControlCmd { Arm, Disarm, Silent, Alarm }

impl ControlCmd {
    /// Маппинг кнопок пульта: 0=Arm, 1=Disarm, 2=Silent, 3=Alarm.
    pub fn from_button_idx(idx: usize) -> Option<Self> {
        match idx {
            0 => Some(Self::Arm),
            1 => Some(Self::Disarm),
            2 => Some(Self::Silent),
            3 => Some(Self::Alarm),
            _ => None,
        }
    }
}
