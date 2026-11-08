use std::{
    sync::mpsc::{self, Receiver, SyncSender},
    time::{Duration, Instant},
};

pub struct LightCommand {
    brightness: u8,
    warmth: u8,
    deadline: Instant,
    reply: SyncSender<Result<(), String>>,
}

pub struct LightReply {
    deadline: Instant,
    result: Receiver<Result<(), String>>,
}

impl LightCommand {
    pub fn new(brightness: u8, warmth: u8) -> (Self, LightReply) {
        let deadline = Instant::now() + Duration::from_secs(1);
        let (reply, result) = mpsc::sync_channel(1);
        (
            Self {
                brightness,
                warmth,
                deadline,
                reply,
            },
            LightReply { deadline, result },
        )
    }

    /// Expiry suppresses queued work. It does not cancel an operation already started.
    pub fn execute(self, now: Instant, apply: impl FnOnce(u8, u8) -> Result<(), String>) {
        let result = if now >= self.deadline {
            Err("Frontlight command expired before execution".into())
        } else {
            apply(self.brightness, self.warmth)
        };
        let _ = self.reply.try_send(result);
    }
}

impl LightReply {
    pub fn wait(self) -> Result<(), String> {
        self.result
            .recv_timeout(self.deadline.saturating_duration_since(Instant::now()))
            .map_err(|_| "Frontlight result unknown: timeout or worker stopped".to_string())?
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expired_commands_never_call_the_driver() {
        for late in [Duration::ZERO, Duration::from_secs(10)] {
            let (command, reply) = LightCommand::new(8, 2);
            let now = command.deadline + late;
            command.execute(now, |_, _| panic!("expired command reached I2C"));
            assert_eq!(
                reply.wait().unwrap_err(),
                "Frontlight command expired before execution"
            );
        }
    }

    #[test]
    fn live_commands_forward_settings_and_driver_results() {
        for outcome in [Ok(()), Err("bus recovery failed".into())] {
            let (command, reply) = LightCommand::new(8, 2);
            let now = command.deadline - Duration::from_nanos(1);
            command.execute(now, |brightness, warmth| {
                assert_eq!((brightness, warmth), (8, 2));
                outcome.clone()
            });
            assert_eq!(reply.wait(), outcome);
        }
    }

    #[test]
    fn timed_out_queued_command_cannot_later_change_the_light() {
        let (mut command, mut reply) = LightCommand::new(0, 0);
        let deadline = Instant::now();
        command.deadline = deadline;
        reply.deadline = deadline;
        let (queue, worker) = mpsc::sync_channel(4);
        queue.try_send(command).unwrap();
        assert_eq!(
            reply.wait().unwrap_err(),
            "Frontlight result unknown: timeout or worker stopped"
        );
        worker.recv().unwrap().execute(Instant::now(), |_, _| {
            panic!("timed out queued command reached I2C")
        });
    }
}
