window.BENCHMARK_DATA = {
  "lastUpdate": 1794588564543,
  "repoUrl": "https://github.com/moq-dev/moq",
  "entries": {
    "MoQ nightly throughput": [
      {
        "commit": {
          "author": {
            "name": "Luke Curley",
            "username": "kixelated",
            "email": "kixelated@gmail.com"
          },
          "committer": {
            "name": "GitHub",
            "username": "web-flow",
            "email": "noreply@github.com"
          },
          "id": "f13283b302ea86fb2f76af59f75489ab9a262c80",
          "message": "feat(demo): add a CARLA teleop demo (#5621)\n\nCo-authored-by: Claude Opus 5.5 <noreply@anthropic.com>\nCo-authored-by: Luke Curley <432854+kixelated@users.noreply.github.com>\nCo-authored-by: GPT-5 <noreply@openai.com>",
          "timestamp": "2026-10-20T08:39:45Z",
          "url": "https://github.com/moq-dev/moq/commit/f13283b302ea86fb2f76af59f75489ab9a262c80"
        },
        "date": 1792501729806,
        "tool": "customBiggerIsBetter",
        "benches": [
          {
            "name": "bench-ffi/rust-net/throughput/size=100/pub=1/sub=1",
            "value": 330.61717879371423,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/rust-net/throughput/size=1024/pub=1/sub=1",
            "value": 1864.3908258919935,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/rust-net/throughput/size=65536/pub=1/sub=1",
            "value": 13386.52917542947,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/rust-net/throughput/size=1048576/pub=1/sub=1",
            "value": 13536.15343744316,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/rust-net/throughput/size=1024/pub=1/sub=4",
            "value": 4773.530415276492,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/rust-net/throughput/size=1024/pub=4/sub=1",
            "value": 1781.7785492381206,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/rust-net/throughput/size=1024/pub=4/sub=4",
            "value": 4550.6631106118075,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/rust-ffi/throughput/size=100/pub=1/sub=1",
            "value": 68.57644331298096,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/rust-ffi/throughput/size=1024/pub=1/sub=1",
            "value": 576.9053249794918,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/rust-ffi/throughput/size=65536/pub=1/sub=1",
            "value": 6949.068050052492,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/rust-ffi/throughput/size=1048576/pub=1/sub=1",
            "value": 7498.932208324513,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/rust-ffi/throughput/size=1024/pub=1/sub=4",
            "value": 742.4256301338369,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/rust-ffi/throughput/size=1024/pub=4/sub=1",
            "value": 546.3557964426078,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/rust-ffi/throughput/size=1024/pub=4/sub=4",
            "value": 706.7389730720178,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/cpp/throughput/size=100/pub=1/sub=1",
            "value": 22.6973,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/cpp/throughput/size=1024/pub=1/sub=1",
            "value": 65.0804,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/cpp/throughput/size=65536/pub=1/sub=1",
            "value": 82.4462,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/cpp/throughput/size=1048576/pub=1/sub=1",
            "value": 76.6543,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/cpp/throughput/size=1024/pub=1/sub=4",
            "value": 67.3545,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/cpp/throughput/size=1024/pub=4/sub=1",
            "value": 66.4884,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/cpp/throughput/size=1024/pub=4/sub=4",
            "value": 67.6273,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/dart/throughput/size=100/pub=1/sub=1",
            "value": 7.878258191352555,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/dart/throughput/size=1024/pub=1/sub=1",
            "value": 66.91970569545633,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/dart/throughput/size=65536/pub=1/sub=1",
            "value": 324.824124434807,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/dart/throughput/size=1048576/pub=1/sub=1",
            "value": 249.9026737171371,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/dart/throughput/size=1024/pub=1/sub=4",
            "value": 93.92616427500664,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/dart/throughput/size=1024/pub=4/sub=1",
            "value": 75.5348243626966,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/dart/throughput/size=1024/pub=4/sub=4",
            "value": 98.23303577568466,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/go/throughput/size=100/pub=1/sub=1",
            "value": 8.510143280415337,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/go/throughput/size=1024/pub=1/sub=1",
            "value": 75.61966349493792,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/go/throughput/size=1024/pub=1/sub=4",
            "value": 179.9551313807728,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/go/throughput/size=1024/pub=4/sub=1",
            "value": 139.92725347415907,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/go/throughput/size=1024/pub=4/sub=4",
            "value": 255.09232390058236,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/go/throughput/size=1048576/pub=1/sub=1",
            "value": 359.6937203472361,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/go/throughput/size=65536/pub=1/sub=1",
            "value": 375.06165422718584,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/kotlin/throughput/size=100/pub=1/sub=1",
            "value": 1.2201502247878746,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/kotlin/throughput/size=1024/pub=1/sub=1",
            "value": 15.26567024796719,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/kotlin/throughput/size=65536/pub=1/sub=1",
            "value": 206.7443329341433,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/kotlin/throughput/size=1048576/pub=1/sub=1",
            "value": 333.2107246728338,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/kotlin/throughput/size=1024/pub=1/sub=4",
            "value": 26.270390812226157,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/kotlin/throughput/size=1024/pub=4/sub=1",
            "value": 14.19046995386741,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/kotlin/throughput/size=1024/pub=4/sub=4",
            "value": 20.89563526470326,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/python/throughput/size=100/pub=1/sub=1",
            "value": 1.1255339297791611,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/python/throughput/size=1024/pub=1/sub=1",
            "value": 10.293224316676707,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/python/throughput/size=65536/pub=1/sub=1",
            "value": 215.9159136936877,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/python/throughput/size=1048576/pub=1/sub=1",
            "value": 342.8043642694111,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/python/throughput/size=1024/pub=1/sub=4",
            "value": 17.54562608035285,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/python/throughput/size=1024/pub=4/sub=1",
            "value": 12.209100429732274,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/python/throughput/size=1024/pub=4/sub=4",
            "value": 19.94464761894559,
            "unit": "MB/s"
          }
        ]
      },
      {
        "commit": {
          "author": {
            "name": "Luke Curley",
            "username": "kixelated",
            "email": "kixelated@gmail.com"
          },
          "committer": {
            "name": "GitHub",
            "username": "web-flow",
            "email": "noreply@github.com"
          },
          "id": "f13283b302ea86fb2f76af59f75489ab9a262c80",
          "message": "feat(demo): add a CARLA teleop demo (#5621)\n\nCo-authored-by: Claude Opus 5.5 <noreply@anthropic.com>\nCo-authored-by: Luke Curley <432854+kixelated@users.noreply.github.com>\nCo-authored-by: GPT-5 <noreply@openai.com>",
          "timestamp": "2026-10-20T08:39:45Z",
          "url": "https://github.com/moq-dev/moq/commit/f13283b302ea86fb2f76af59f75489ab9a262c80"
        },
        "date": 1792520372361,
        "tool": "customBiggerIsBetter",
        "benches": [
          {
            "name": "bench-ffi/rust-net/throughput/size=100/pub=1/sub=1",
            "value": 330.61717879371423,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/rust-net/throughput/size=1024/pub=1/sub=1",
            "value": 1864.3908258919935,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/rust-net/throughput/size=65536/pub=1/sub=1",
            "value": 13386.52917542947,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/rust-net/throughput/size=1048576/pub=1/sub=1",
            "value": 13536.15343744316,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/rust-net/throughput/size=1024/pub=1/sub=4",
            "value": 4773.530415276492,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/rust-net/throughput/size=1024/pub=4/sub=1",
            "value": 1781.7785492381206,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/rust-net/throughput/size=1024/pub=4/sub=4",
            "value": 4550.6631106118075,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/rust-ffi/throughput/size=100/pub=1/sub=1",
            "value": 68.57644331298096,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/rust-ffi/throughput/size=1024/pub=1/sub=1",
            "value": 576.9053249794918,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/rust-ffi/throughput/size=65536/pub=1/sub=1",
            "value": 6949.068050052492,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/rust-ffi/throughput/size=1048576/pub=1/sub=1",
            "value": 7498.932208324513,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/rust-ffi/throughput/size=1024/pub=1/sub=4",
            "value": 742.4256301338369,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/rust-ffi/throughput/size=1024/pub=4/sub=1",
            "value": 546.3557964426078,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/rust-ffi/throughput/size=1024/pub=4/sub=4",
            "value": 706.7389730720178,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/cpp/throughput/size=100/pub=1/sub=1",
            "value": 22.6973,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/cpp/throughput/size=1024/pub=1/sub=1",
            "value": 65.0804,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/cpp/throughput/size=65536/pub=1/sub=1",
            "value": 82.4462,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/cpp/throughput/size=1048576/pub=1/sub=1",
            "value": 76.6543,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/cpp/throughput/size=1024/pub=1/sub=4",
            "value": 67.3545,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/cpp/throughput/size=1024/pub=4/sub=1",
            "value": 66.4884,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/cpp/throughput/size=1024/pub=4/sub=4",
            "value": 67.6273,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/dart/throughput/size=100/pub=1/sub=1",
            "value": 7.878258191352555,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/dart/throughput/size=1024/pub=1/sub=1",
            "value": 66.91970569545633,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/dart/throughput/size=65536/pub=1/sub=1",
            "value": 324.824124434807,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/dart/throughput/size=1048576/pub=1/sub=1",
            "value": 249.9026737171371,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/dart/throughput/size=1024/pub=1/sub=4",
            "value": 93.92616427500664,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/dart/throughput/size=1024/pub=4/sub=1",
            "value": 75.5348243626966,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/dart/throughput/size=1024/pub=4/sub=4",
            "value": 98.23303577568466,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/go/throughput/size=100/pub=1/sub=1",
            "value": 8.510143280415337,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/go/throughput/size=1024/pub=1/sub=1",
            "value": 75.61966349493792,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/go/throughput/size=1024/pub=1/sub=4",
            "value": 179.9551313807728,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/go/throughput/size=1024/pub=4/sub=1",
            "value": 139.92725347415907,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/go/throughput/size=1024/pub=4/sub=4",
            "value": 255.09232390058236,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/go/throughput/size=1048576/pub=1/sub=1",
            "value": 359.6937203472361,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/go/throughput/size=65536/pub=1/sub=1",
            "value": 375.06165422718584,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/kotlin/throughput/size=100/pub=1/sub=1",
            "value": 1.2201502247878746,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/kotlin/throughput/size=1024/pub=1/sub=1",
            "value": 15.26567024796719,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/kotlin/throughput/size=65536/pub=1/sub=1",
            "value": 206.7443329341433,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/kotlin/throughput/size=1048576/pub=1/sub=1",
            "value": 333.2107246728338,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/kotlin/throughput/size=1024/pub=1/sub=4",
            "value": 26.270390812226157,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/kotlin/throughput/size=1024/pub=4/sub=1",
            "value": 14.19046995386741,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/kotlin/throughput/size=1024/pub=4/sub=4",
            "value": 20.89563526470326,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/python/throughput/size=100/pub=1/sub=1",
            "value": 1.1255339297791611,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/python/throughput/size=1024/pub=1/sub=1",
            "value": 10.293224316676707,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/python/throughput/size=65536/pub=1/sub=1",
            "value": 215.9159136936877,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/python/throughput/size=1048576/pub=1/sub=1",
            "value": 342.8043642694111,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/python/throughput/size=1024/pub=1/sub=4",
            "value": 17.54562608035285,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/python/throughput/size=1024/pub=4/sub=1",
            "value": 12.209100429732274,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/python/throughput/size=1024/pub=4/sub=4",
            "value": 19.94464761894559,
            "unit": "MB/s"
          }
        ]
      },
      {
        "commit": {
          "author": {
            "name": "Luke Curley",
            "username": "kixelated",
            "email": "kixelated@gmail.com"
          },
          "committer": {
            "name": "GitHub",
            "username": "web-flow",
            "email": "noreply@github.com"
          },
          "id": "f13283b302ea86fb2f76af59f75489ab9a262c80",
          "message": "feat(demo): add a CARLA teleop demo (#5621)\n\nCo-authored-by: Claude Opus 5.5 <noreply@anthropic.com>\nCo-authored-by: Luke Curley <432854+kixelated@users.noreply.github.com>\nCo-authored-by: GPT-5 <noreply@openai.com>",
          "timestamp": "2026-10-20T08:39:45Z",
          "url": "https://github.com/moq-dev/moq/commit/f13283b302ea86fb2f76af59f75489ab9a262c80"
        },
        "date": 1792520489882,
        "tool": "customBiggerIsBetter",
        "benches": [
          {
            "name": "bench-ffi/rust-net/throughput/size=100/pub=1/sub=1",
            "value": 330.61717879371423,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/rust-net/throughput/size=1024/pub=1/sub=1",
            "value": 1864.3908258919935,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/rust-net/throughput/size=65536/pub=1/sub=1",
            "value": 13386.52917542947,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/rust-net/throughput/size=1048576/pub=1/sub=1",
            "value": 13536.15343744316,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/rust-net/throughput/size=1024/pub=1/sub=4",
            "value": 4773.530415276492,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/rust-net/throughput/size=1024/pub=4/sub=1",
            "value": 1781.7785492381206,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/rust-net/throughput/size=1024/pub=4/sub=4",
            "value": 4550.6631106118075,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/rust-ffi/throughput/size=100/pub=1/sub=1",
            "value": 68.57644331298096,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/rust-ffi/throughput/size=1024/pub=1/sub=1",
            "value": 576.9053249794918,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/rust-ffi/throughput/size=65536/pub=1/sub=1",
            "value": 6949.068050052492,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/rust-ffi/throughput/size=1048576/pub=1/sub=1",
            "value": 7498.932208324513,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/rust-ffi/throughput/size=1024/pub=1/sub=4",
            "value": 742.4256301338369,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/rust-ffi/throughput/size=1024/pub=4/sub=1",
            "value": 546.3557964426078,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/rust-ffi/throughput/size=1024/pub=4/sub=4",
            "value": 706.7389730720178,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/cpp/throughput/size=100/pub=1/sub=1",
            "value": 22.6973,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/cpp/throughput/size=1024/pub=1/sub=1",
            "value": 65.0804,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/cpp/throughput/size=65536/pub=1/sub=1",
            "value": 82.4462,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/cpp/throughput/size=1048576/pub=1/sub=1",
            "value": 76.6543,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/cpp/throughput/size=1024/pub=1/sub=4",
            "value": 67.3545,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/cpp/throughput/size=1024/pub=4/sub=1",
            "value": 66.4884,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/cpp/throughput/size=1024/pub=4/sub=4",
            "value": 67.6273,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/dart/throughput/size=100/pub=1/sub=1",
            "value": 7.878258191352555,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/dart/throughput/size=1024/pub=1/sub=1",
            "value": 66.91970569545633,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/dart/throughput/size=65536/pub=1/sub=1",
            "value": 324.824124434807,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/dart/throughput/size=1048576/pub=1/sub=1",
            "value": 249.9026737171371,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/dart/throughput/size=1024/pub=1/sub=4",
            "value": 93.92616427500664,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/dart/throughput/size=1024/pub=4/sub=1",
            "value": 75.5348243626966,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/dart/throughput/size=1024/pub=4/sub=4",
            "value": 98.23303577568466,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/go/throughput/size=100/pub=1/sub=1",
            "value": 8.510143280415337,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/go/throughput/size=1024/pub=1/sub=1",
            "value": 75.61966349493792,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/go/throughput/size=1024/pub=1/sub=4",
            "value": 179.9551313807728,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/go/throughput/size=1024/pub=4/sub=1",
            "value": 139.92725347415907,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/go/throughput/size=1024/pub=4/sub=4",
            "value": 255.09232390058236,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/go/throughput/size=1048576/pub=1/sub=1",
            "value": 359.6937203472361,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/go/throughput/size=65536/pub=1/sub=1",
            "value": 375.06165422718584,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/kotlin/throughput/size=100/pub=1/sub=1",
            "value": 1.2201502247878746,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/kotlin/throughput/size=1024/pub=1/sub=1",
            "value": 15.26567024796719,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/kotlin/throughput/size=65536/pub=1/sub=1",
            "value": 206.7443329341433,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/kotlin/throughput/size=1048576/pub=1/sub=1",
            "value": 333.2107246728338,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/kotlin/throughput/size=1024/pub=1/sub=4",
            "value": 26.270390812226157,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/kotlin/throughput/size=1024/pub=4/sub=1",
            "value": 14.19046995386741,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/kotlin/throughput/size=1024/pub=4/sub=4",
            "value": 20.89563526470326,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/python/throughput/size=100/pub=1/sub=1",
            "value": 1.1255339297791611,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/python/throughput/size=1024/pub=1/sub=1",
            "value": 10.293224316676707,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/python/throughput/size=65536/pub=1/sub=1",
            "value": 215.9159136936877,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/python/throughput/size=1048576/pub=1/sub=1",
            "value": 342.8043642694111,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/python/throughput/size=1024/pub=1/sub=4",
            "value": 17.54562608035285,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/python/throughput/size=1024/pub=4/sub=1",
            "value": 12.209100429732274,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/python/throughput/size=1024/pub=4/sub=4",
            "value": 19.94464761894559,
            "unit": "MB/s"
          }
        ]
      },
      {
        "commit": {
          "author": {
            "name": "Melanie Riise",
            "username": "mriise",
            "email": "me@mriise.net"
          },
          "committer": {
            "name": "GitHub",
            "username": "web-flow",
            "email": "noreply@github.com"
          },
          "id": "bc64b9c52561209322dcb8dec9992bf867f499f7",
          "message": "fix(net): refuse a revoked subscription while its SUBSCRIBE is still pending (#6061)\n\nCo-authored-by: Claude Fable 5.1 <noreply@anthropic.com>\nCo-authored-by: Luke Curley <kixelated@gmail.com>",
          "timestamp": "2026-10-21T05:04:56Z",
          "url": "https://github.com/moq-dev/moq/commit/bc64b9c52561209322dcb8dec9992bf867f499f7"
        },
        "date": 1792589089380,
        "tool": "customBiggerIsBetter",
        "benches": [
          {
            "name": "bench-ffi/rust-net/throughput/size=100/pub=1/sub=1",
            "value": 309.3294828850396,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/rust-net/throughput/size=1024/pub=1/sub=1",
            "value": 1693.2144899606355,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/rust-net/throughput/size=65536/pub=1/sub=1",
            "value": 13121.348576875549,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/rust-net/throughput/size=1048576/pub=1/sub=1",
            "value": 12930.840076190672,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/rust-net/throughput/size=1024/pub=1/sub=4",
            "value": 4591.929660828687,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/rust-net/throughput/size=1024/pub=4/sub=1",
            "value": 1440.1863917549833,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/rust-net/throughput/size=1024/pub=4/sub=4",
            "value": 4089.450256411346,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/rust-ffi/throughput/size=100/pub=1/sub=1",
            "value": 69.00923986155097,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/rust-ffi/throughput/size=1024/pub=1/sub=1",
            "value": 573.8050708612742,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/rust-ffi/throughput/size=65536/pub=1/sub=1",
            "value": 6770.954200328875,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/rust-ffi/throughput/size=1048576/pub=1/sub=1",
            "value": 6653.394355687621,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/rust-ffi/throughput/size=1024/pub=1/sub=4",
            "value": 739.584759589979,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/rust-ffi/throughput/size=1024/pub=4/sub=1",
            "value": 527.5857041911794,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/rust-ffi/throughput/size=1024/pub=4/sub=4",
            "value": 703.7809192604783,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/cpp/throughput/size=100/pub=1/sub=1",
            "value": 22.8466,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/cpp/throughput/size=1024/pub=1/sub=1",
            "value": 65.0058,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/cpp/throughput/size=65536/pub=1/sub=1",
            "value": 82.0986,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/cpp/throughput/size=1048576/pub=1/sub=1",
            "value": 76.1506,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/cpp/throughput/size=1024/pub=1/sub=4",
            "value": 67.287,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/cpp/throughput/size=1024/pub=4/sub=1",
            "value": 66.1839,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/cpp/throughput/size=1024/pub=4/sub=4",
            "value": 67.4222,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/dart/throughput/size=100/pub=1/sub=1",
            "value": 9.470939254803463,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/dart/throughput/size=1024/pub=1/sub=1",
            "value": 75.91909956031502,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/dart/throughput/size=65536/pub=1/sub=1",
            "value": 263.2174991458219,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/dart/throughput/size=1048576/pub=1/sub=1",
            "value": 208.79732695072462,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/dart/throughput/size=1024/pub=1/sub=4",
            "value": 110.7332142985536,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/dart/throughput/size=1024/pub=4/sub=1",
            "value": 84.42893452389855,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/dart/throughput/size=1024/pub=4/sub=4",
            "value": 114.15437411983261,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/go/throughput/size=100/pub=1/sub=1",
            "value": 8.958541219437958,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/go/throughput/size=1024/pub=1/sub=1",
            "value": 78.15264837396677,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/go/throughput/size=1024/pub=1/sub=4",
            "value": 203.090572208818,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/go/throughput/size=1024/pub=4/sub=1",
            "value": 136.82860866010185,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/go/throughput/size=1024/pub=4/sub=4",
            "value": 220.5088931812131,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/go/throughput/size=1048576/pub=1/sub=1",
            "value": 368.8718363093468,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/go/throughput/size=65536/pub=1/sub=1",
            "value": 369.95216879236466,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/kotlin/throughput/size=100/pub=1/sub=1",
            "value": 1.0299207233016494,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/kotlin/throughput/size=1024/pub=1/sub=1",
            "value": 13.623504694366623,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/kotlin/throughput/size=65536/pub=1/sub=1",
            "value": 211.07132540226806,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/kotlin/throughput/size=1048576/pub=1/sub=1",
            "value": 449.79315777574067,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/kotlin/throughput/size=1024/pub=1/sub=4",
            "value": 26.114897045392546,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/kotlin/throughput/size=1024/pub=4/sub=1",
            "value": 14.875761099080242,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/kotlin/throughput/size=1024/pub=4/sub=4",
            "value": 19.259224050419736,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/python/throughput/size=100/pub=1/sub=1",
            "value": 1.2873199202936298,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/python/throughput/size=1024/pub=1/sub=1",
            "value": 11.611788465030175,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/python/throughput/size=65536/pub=1/sub=1",
            "value": 224.60740709567045,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/python/throughput/size=1048576/pub=1/sub=1",
            "value": 345.7455099905332,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/python/throughput/size=1024/pub=1/sub=4",
            "value": 19.66044002639729,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/python/throughput/size=1024/pub=4/sub=1",
            "value": 13.421516137663613,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/python/throughput/size=1024/pub=4/sub=4",
            "value": 22.407625383024705,
            "unit": "MB/s"
          }
        ]
      },
      {
        "commit": {
          "author": {
            "name": "Luke Curley",
            "username": "kixelated",
            "email": "kixelated@gmail.com"
          },
          "committer": {
            "name": "GitHub",
            "username": "web-flow",
            "email": "noreply@github.com"
          },
          "id": "33940b6e336133a64d8601f42833f91708a7fd35",
          "message": "test(cli): cover listener admission deadlines in process (#6234)\n\nCo-authored-by: GPT-6 <noreply@openai.com>\nCo-authored-by: Claude Opus 5.5 <noreply@anthropic.com>",
          "timestamp": "2026-10-22T11:33:11Z",
          "url": "https://github.com/moq-dev/moq/commit/33940b6e336133a64d8601f42833f91708a7fd35"
        },
        "date": 1792677140315,
        "tool": "customBiggerIsBetter",
        "benches": [
          {
            "name": "bench-ffi/rust-net/throughput/size=100/pub=1/sub=1",
            "value": 270.4881344952873,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/rust-net/throughput/size=1024/pub=1/sub=1",
            "value": 1755.735840648112,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/rust-net/throughput/size=65536/pub=1/sub=1",
            "value": 13782.948836293244,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/rust-net/throughput/size=1048576/pub=1/sub=1",
            "value": 13743.372190313807,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/rust-net/throughput/size=1024/pub=1/sub=4",
            "value": 4494.171047318294,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/rust-net/throughput/size=1024/pub=4/sub=1",
            "value": 1680.6309674220038,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/rust-net/throughput/size=1024/pub=4/sub=4",
            "value": 4363.595301439181,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/rust-ffi/throughput/size=100/pub=1/sub=1",
            "value": 67.43395219676479,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/rust-ffi/throughput/size=1024/pub=1/sub=1",
            "value": 575.2687410223352,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/rust-ffi/throughput/size=65536/pub=1/sub=1",
            "value": 7331.481703502511,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/rust-ffi/throughput/size=1048576/pub=1/sub=1",
            "value": 7867.946985682597,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/rust-ffi/throughput/size=1024/pub=1/sub=4",
            "value": 746.9347276744635,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/rust-ffi/throughput/size=1024/pub=4/sub=1",
            "value": 557.6553219098396,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/rust-ffi/throughput/size=1024/pub=4/sub=4",
            "value": 723.3713074219598,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/cpp/throughput/size=100/pub=1/sub=1",
            "value": 22.7383,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/cpp/throughput/size=1024/pub=1/sub=1",
            "value": 64.9541,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/cpp/throughput/size=65536/pub=1/sub=1",
            "value": 82.4395,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/cpp/throughput/size=1048576/pub=1/sub=1",
            "value": 77.0724,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/cpp/throughput/size=1024/pub=1/sub=4",
            "value": 67.3578,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/cpp/throughput/size=1024/pub=4/sub=1",
            "value": 66.4434,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/cpp/throughput/size=1024/pub=4/sub=4",
            "value": 67.7525,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/dart/throughput/size=100/pub=1/sub=1",
            "value": 9.847689548227644,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/dart/throughput/size=1024/pub=1/sub=1",
            "value": 83.98330672738886,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/dart/throughput/size=65536/pub=1/sub=1",
            "value": 340.1087452682043,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/dart/throughput/size=1048576/pub=1/sub=1",
            "value": 257.89804151807147,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/dart/throughput/size=1024/pub=1/sub=4",
            "value": 123.20521589568209,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/dart/throughput/size=1024/pub=4/sub=1",
            "value": 95.32213887482007,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/dart/throughput/size=1024/pub=4/sub=4",
            "value": 126.72613154667019,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/go/throughput/size=100/pub=1/sub=1",
            "value": 8.636144432195568,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/go/throughput/size=1024/pub=1/sub=1",
            "value": 75.8465007998364,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/go/throughput/size=1024/pub=1/sub=4",
            "value": 201.77527526303933,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/go/throughput/size=1024/pub=4/sub=1",
            "value": 141.58210109432073,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/go/throughput/size=1024/pub=4/sub=4",
            "value": 261.556898269975,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/go/throughput/size=1048576/pub=1/sub=1",
            "value": 351.7567446861209,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/go/throughput/size=65536/pub=1/sub=1",
            "value": 386.02930935711504,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/kotlin/throughput/size=100/pub=1/sub=1",
            "value": 1.1104356138399525,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/kotlin/throughput/size=1024/pub=1/sub=1",
            "value": 14.443692802588332,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/kotlin/throughput/size=65536/pub=1/sub=1",
            "value": 239.91555803779988,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/kotlin/throughput/size=1048576/pub=1/sub=1",
            "value": 324.83859961316597,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/kotlin/throughput/size=1024/pub=1/sub=4",
            "value": 26.47405737481137,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/kotlin/throughput/size=1024/pub=4/sub=1",
            "value": 14.759806036563829,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/kotlin/throughput/size=1024/pub=4/sub=4",
            "value": 18.894786802516435,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/python/throughput/size=100/pub=1/sub=1",
            "value": 1.2531183293697403,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/python/throughput/size=1024/pub=1/sub=1",
            "value": 11.61376257338948,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/python/throughput/size=65536/pub=1/sub=1",
            "value": 273.926524014353,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/python/throughput/size=1048576/pub=1/sub=1",
            "value": 479.55923662999743,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/python/throughput/size=1024/pub=1/sub=4",
            "value": 20.145000556860186,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/python/throughput/size=1024/pub=4/sub=1",
            "value": 13.445339745169651,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/python/throughput/size=1024/pub=4/sub=4",
            "value": 21.43102843647428,
            "unit": "MB/s"
          }
        ]
      },
      {
        "commit": {
          "author": {
            "name": "Luke Curley",
            "username": "kixelated",
            "email": "kixelated@gmail.com"
          },
          "committer": {
            "name": "GitHub",
            "username": "web-flow",
            "email": "noreply@github.com"
          },
          "id": "4381af6f07e32d594036723f80d298b67e60085f",
          "message": "docs(go): define deterministic reservation release with Update(0) (#6424)\n\nCo-authored-by: GPT-6 <noreply@openai.com>",
          "timestamp": "2026-10-23T09:04:12Z",
          "url": "https://github.com/moq-dev/moq/commit/4381af6f07e32d594036723f80d298b67e60085f"
        },
        "date": 1792761476725,
        "tool": "customBiggerIsBetter",
        "benches": [
          {
            "name": "bench-ffi/rust-net/throughput/size=100/pub=1/sub=1",
            "value": 265.0530775247672,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/rust-net/throughput/size=1024/pub=1/sub=1",
            "value": 1627.5535369387355,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/rust-net/throughput/size=65536/pub=1/sub=1",
            "value": 12952.915287528189,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/rust-net/throughput/size=1048576/pub=1/sub=1",
            "value": 13156.184645964227,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/rust-net/throughput/size=1024/pub=1/sub=4",
            "value": 4424.715688522233,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/rust-net/throughput/size=1024/pub=4/sub=1",
            "value": 1483.2562308293377,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/rust-net/throughput/size=1024/pub=4/sub=4",
            "value": 4093.5394129659767,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/rust-ffi/throughput/size=100/pub=1/sub=1",
            "value": 65.61700393011247,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/rust-ffi/throughput/size=1024/pub=1/sub=1",
            "value": 549.1763876343099,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/rust-ffi/throughput/size=65536/pub=1/sub=1",
            "value": 6857.11635649123,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/rust-ffi/throughput/size=1048576/pub=1/sub=1",
            "value": 7022.892354843222,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/rust-ffi/throughput/size=1024/pub=1/sub=4",
            "value": 729.0912493552148,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/rust-ffi/throughput/size=1024/pub=4/sub=1",
            "value": 522.8568017671663,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/rust-ffi/throughput/size=1024/pub=4/sub=4",
            "value": 697.101006186387,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/cpp/throughput/size=100/pub=1/sub=1",
            "value": 22.4812,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/cpp/throughput/size=1024/pub=1/sub=1",
            "value": 64.2783,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/cpp/throughput/size=65536/pub=1/sub=1",
            "value": 82.6333,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/cpp/throughput/size=1048576/pub=1/sub=1",
            "value": 76.6362,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/cpp/throughput/size=1024/pub=1/sub=4",
            "value": 67.0902,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/cpp/throughput/size=1024/pub=4/sub=1",
            "value": 65.9293,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/cpp/throughput/size=1024/pub=4/sub=4",
            "value": 67.3498,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/dart/throughput/size=100/pub=1/sub=1",
            "value": 8.995242669822892,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/dart/throughput/size=1024/pub=1/sub=1",
            "value": 72.88173209794759,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/dart/throughput/size=65536/pub=1/sub=1",
            "value": 263.86540765949286,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/dart/throughput/size=1048576/pub=1/sub=1",
            "value": 209.2504547400224,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/dart/throughput/size=1024/pub=1/sub=4",
            "value": 108.86020118571197,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/dart/throughput/size=1024/pub=4/sub=1",
            "value": 83.54089634902907,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/dart/throughput/size=1024/pub=4/sub=4",
            "value": 115.0579013467587,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/go/throughput/size=100/pub=1/sub=1",
            "value": 9.363908280816027,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/go/throughput/size=1024/pub=1/sub=1",
            "value": 79.53703731420524,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/go/throughput/size=1024/pub=1/sub=4",
            "value": 201.22344124455947,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/go/throughput/size=1024/pub=4/sub=1",
            "value": 140.03227257050196,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/go/throughput/size=1024/pub=4/sub=4",
            "value": 251.84103398189052,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/go/throughput/size=1048576/pub=1/sub=1",
            "value": 479.99506971791277,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/go/throughput/size=65536/pub=1/sub=1",
            "value": 458.608609770824,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/kotlin/throughput/size=100/pub=1/sub=1",
            "value": 1.0222957695697894,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/kotlin/throughput/size=1024/pub=1/sub=1",
            "value": 14.632847897033683,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/kotlin/throughput/size=65536/pub=1/sub=1",
            "value": 227.53295331143732,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/kotlin/throughput/size=1048576/pub=1/sub=1",
            "value": 337.2355536695705,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/kotlin/throughput/size=1024/pub=1/sub=4",
            "value": 29.19253720539338,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/kotlin/throughput/size=1024/pub=4/sub=1",
            "value": 14.321099514366418,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/kotlin/throughput/size=1024/pub=4/sub=4",
            "value": 20.085717656624837,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/python/throughput/size=100/pub=1/sub=1",
            "value": 1.3039578749934406,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/python/throughput/size=1024/pub=1/sub=1",
            "value": 12.142528832452934,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/python/throughput/size=65536/pub=1/sub=1",
            "value": 228.74045258042497,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/python/throughput/size=1048576/pub=1/sub=1",
            "value": 345.62517745065065,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/python/throughput/size=1024/pub=1/sub=4",
            "value": 21.559310869641823,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/python/throughput/size=1024/pub=4/sub=1",
            "value": 14.048873262436238,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/python/throughput/size=1024/pub=4/sub=4",
            "value": 23.466458890729445,
            "unit": "MB/s"
          }
        ]
      },
      {
        "commit": {
          "author": {
            "name": "Luke Curley",
            "username": "kixelated",
            "email": "kixelated@gmail.com"
          },
          "committer": {
            "name": "GitHub",
            "username": "web-flow",
            "email": "noreply@github.com"
          },
          "id": "0513c07369357ae0ecd6109d0f6afc61d16b597f",
          "message": "fix(net)!: ranges refuse what the wire drops (#6519)\n\nCo-authored-by: Luke Curley <432854+kixelated@users.noreply.github.com>\nCo-authored-by: Claude Opus 5.5 <noreply@anthropic.com>",
          "timestamp": "2026-10-24T09:10:41Z",
          "url": "https://github.com/moq-dev/moq/commit/0513c07369357ae0ecd6109d0f6afc61d16b597f"
        },
        "date": 1792845817678,
        "tool": "customBiggerIsBetter",
        "benches": [
          {
            "name": "bench-ffi/rust-net/throughput/size=100/pub=1/sub=1",
            "value": 274.8124417285045,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/rust-net/throughput/size=1024/pub=1/sub=1",
            "value": 1629.8091902375106,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/rust-net/throughput/size=65536/pub=1/sub=1",
            "value": 12446.823514425781,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/rust-net/throughput/size=1048576/pub=1/sub=1",
            "value": 12808.08752686475,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/rust-net/throughput/size=1024/pub=1/sub=4",
            "value": 4451.492219140088,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/rust-net/throughput/size=1024/pub=4/sub=1",
            "value": 1436.6496078761727,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/rust-net/throughput/size=1024/pub=4/sub=4",
            "value": 4014.1738958709143,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/rust-ffi/throughput/size=100/pub=1/sub=1",
            "value": 65.42522298061196,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/rust-ffi/throughput/size=1024/pub=1/sub=1",
            "value": 551.660867532652,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/rust-ffi/throughput/size=65536/pub=1/sub=1",
            "value": 6863.667132293942,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/rust-ffi/throughput/size=1048576/pub=1/sub=1",
            "value": 7181.126355930812,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/rust-ffi/throughput/size=1024/pub=1/sub=4",
            "value": 721.5484115980297,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/rust-ffi/throughput/size=1024/pub=4/sub=1",
            "value": 520.2598374927071,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/rust-ffi/throughput/size=1024/pub=4/sub=4",
            "value": 692.0579814014745,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/cpp/throughput/size=100/pub=1/sub=1",
            "value": 22.2778,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/cpp/throughput/size=1024/pub=1/sub=1",
            "value": 64.6282,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/cpp/throughput/size=65536/pub=1/sub=1",
            "value": 82.4501,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/cpp/throughput/size=1048576/pub=1/sub=1",
            "value": 76.606,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/cpp/throughput/size=1024/pub=1/sub=4",
            "value": 67.1696,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/cpp/throughput/size=1024/pub=4/sub=1",
            "value": 65.8639,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/cpp/throughput/size=1024/pub=4/sub=4",
            "value": 67.4077,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/dart/throughput/size=100/pub=1/sub=1",
            "value": 7.621073933135635,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/dart/throughput/size=1024/pub=1/sub=1",
            "value": 61.2544759155259,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/dart/throughput/size=65536/pub=1/sub=1",
            "value": 291.21217531372616,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/dart/throughput/size=1048576/pub=1/sub=1",
            "value": 236.29950548665997,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/dart/throughput/size=1024/pub=1/sub=4",
            "value": 89.0201773432865,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/dart/throughput/size=1024/pub=4/sub=1",
            "value": 69.86250374303795,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/dart/throughput/size=1024/pub=4/sub=4",
            "value": 89.8112855774153,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/go/throughput/size=100/pub=1/sub=1",
            "value": 9.17426579508784,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/go/throughput/size=1024/pub=1/sub=1",
            "value": 76.94467576712324,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/go/throughput/size=1024/pub=1/sub=4",
            "value": 205.8049783631095,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/go/throughput/size=1024/pub=4/sub=1",
            "value": 134.7533169547812,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/go/throughput/size=1024/pub=4/sub=4",
            "value": 242.16671431371216,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/go/throughput/size=1048576/pub=1/sub=1",
            "value": 342.5112642956048,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/go/throughput/size=65536/pub=1/sub=1",
            "value": 369.4377298846058,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/kotlin/throughput/size=100/pub=1/sub=1",
            "value": 0.8937594282330467,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/kotlin/throughput/size=1024/pub=1/sub=1",
            "value": 13.951543272328525,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/kotlin/throughput/size=65536/pub=1/sub=1",
            "value": 265.57564450845734,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/kotlin/throughput/size=1048576/pub=1/sub=1",
            "value": 450.1776685658549,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/kotlin/throughput/size=1024/pub=1/sub=4",
            "value": 21.83613429046926,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/kotlin/throughput/size=1024/pub=4/sub=1",
            "value": 12.585216534991092,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/kotlin/throughput/size=1024/pub=4/sub=4",
            "value": 21.684649905433673,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/python/throughput/size=100/pub=1/sub=1",
            "value": 1.2367414168214794,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/python/throughput/size=1024/pub=1/sub=1",
            "value": 11.215997363256012,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/python/throughput/size=65536/pub=1/sub=1",
            "value": 222.23343495896813,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/python/throughput/size=1048576/pub=1/sub=1",
            "value": 340.6783767063627,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/python/throughput/size=1024/pub=1/sub=4",
            "value": 20.25947737446759,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/python/throughput/size=1024/pub=4/sub=1",
            "value": 13.244073054356639,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/python/throughput/size=1024/pub=4/sub=4",
            "value": 22.217046378630574,
            "unit": "MB/s"
          }
        ]
      },
      {
        "commit": {
          "author": {
            "name": "Luke Curley",
            "username": "kixelated",
            "email": "kixelated@gmail.com"
          },
          "committer": {
            "name": "GitHub",
            "username": "web-flow",
            "email": "noreply@github.com"
          },
          "id": "66af72315bd333d09199ecd0efee699c9e9e23bd",
          "message": "feat(stats)!: publish groups as tracks of one node broadcast (#6428)\n\nCo-authored-by: Luke Curley <432854+kixelated@users.noreply.github.com>\nCo-authored-by: Claude Opus 5.5 <noreply@anthropic.com>",
          "timestamp": "2026-10-25T06:44:52Z",
          "url": "https://github.com/moq-dev/moq/commit/66af72315bd333d09199ecd0efee699c9e9e23bd"
        },
        "date": 1792934182873,
        "tool": "customBiggerIsBetter",
        "benches": [
          {
            "name": "bench-ffi/rust-net/throughput/size=100/pub=1/sub=1",
            "value": 273.2640017181239,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/rust-net/throughput/size=1024/pub=1/sub=1",
            "value": 1743.1945416806489,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/rust-net/throughput/size=65536/pub=1/sub=1",
            "value": 13458.814059980125,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/rust-net/throughput/size=1048576/pub=1/sub=1",
            "value": 13911.630119319221,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/rust-net/throughput/size=1024/pub=1/sub=4",
            "value": 4489.610596618695,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/rust-net/throughput/size=1024/pub=4/sub=1",
            "value": 1699.9299984407278,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/rust-net/throughput/size=1024/pub=4/sub=4",
            "value": 4325.300294268949,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/rust-ffi/throughput/size=100/pub=1/sub=1",
            "value": 65.05035089742736,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/rust-ffi/throughput/size=1024/pub=1/sub=1",
            "value": 547.3347736830398,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/rust-ffi/throughput/size=65536/pub=1/sub=1",
            "value": 6579.095811127843,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/rust-ffi/throughput/size=1048576/pub=1/sub=1",
            "value": 7401.0512050579655,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/rust-ffi/throughput/size=1024/pub=1/sub=4",
            "value": 723.2366303600033,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/rust-ffi/throughput/size=1024/pub=4/sub=1",
            "value": 534.3830455767664,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/rust-ffi/throughput/size=1024/pub=4/sub=4",
            "value": 700.4611811940292,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/cpp/throughput/size=100/pub=1/sub=1",
            "value": 22.1946,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/cpp/throughput/size=1024/pub=1/sub=1",
            "value": 62.9015,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/cpp/throughput/size=65536/pub=1/sub=1",
            "value": 79.4111,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/cpp/throughput/size=1048576/pub=1/sub=1",
            "value": 74.1392,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/cpp/throughput/size=1024/pub=1/sub=4",
            "value": 65.3411,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/cpp/throughput/size=1024/pub=4/sub=1",
            "value": 64.0188,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/cpp/throughput/size=1024/pub=4/sub=4",
            "value": 65.4568,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/dart/throughput/size=100/pub=1/sub=1",
            "value": 7.739116617414411,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/dart/throughput/size=1024/pub=1/sub=1",
            "value": 59.269717322503375,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/dart/throughput/size=65536/pub=1/sub=1",
            "value": 281.6631529465674,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/dart/throughput/size=1048576/pub=1/sub=1",
            "value": 230.06552254755707,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/dart/throughput/size=1024/pub=1/sub=4",
            "value": 83.50427437482088,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/dart/throughput/size=1024/pub=4/sub=1",
            "value": 67.71530870945028,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/dart/throughput/size=1024/pub=4/sub=4",
            "value": 87.99555628374247,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/go/throughput/size=100/pub=1/sub=1",
            "value": 9.126832934812088,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/go/throughput/size=1024/pub=1/sub=1",
            "value": 78.14441798215091,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/go/throughput/size=1024/pub=1/sub=4",
            "value": 197.08735757386796,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/go/throughput/size=1024/pub=4/sub=1",
            "value": 138.07077677307223,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/go/throughput/size=1024/pub=4/sub=4",
            "value": 259.67000202800284,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/go/throughput/size=1048576/pub=1/sub=1",
            "value": 349.7532645022352,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/go/throughput/size=65536/pub=1/sub=1",
            "value": 375.2971456362266,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/kotlin/throughput/size=100/pub=1/sub=1",
            "value": 0.8958277914403403,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/kotlin/throughput/size=1024/pub=1/sub=1",
            "value": 15.33148898937301,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/kotlin/throughput/size=65536/pub=1/sub=1",
            "value": 246.7886821857732,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/kotlin/throughput/size=1048576/pub=1/sub=1",
            "value": 329.81824420810216,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/kotlin/throughput/size=1024/pub=1/sub=4",
            "value": 26.198711189989698,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/kotlin/throughput/size=1024/pub=4/sub=1",
            "value": 9.925315979547179,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/kotlin/throughput/size=1024/pub=4/sub=4",
            "value": 24.458314470929086,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/python/throughput/size=100/pub=1/sub=1",
            "value": 1.3029121365612355,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/python/throughput/size=1024/pub=1/sub=1",
            "value": 12.090159846946571,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/python/throughput/size=65536/pub=1/sub=1",
            "value": 228.1547961064669,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/python/throughput/size=1048576/pub=1/sub=1",
            "value": 352.7317014129771,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/python/throughput/size=1024/pub=1/sub=4",
            "value": 20.973715497467175,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/python/throughput/size=1024/pub=4/sub=1",
            "value": 13.713101027090769,
            "unit": "MB/s"
          },
          {
            "name": "bench-ffi/python/throughput/size=1024/pub=4/sub=4",
            "value": 22.12156591345346,
            "unit": "MB/s"
          }
        ]
      },
      {
        "commit": {
          "author": {
            "name": "Luke Curley",
            "username": "kixelated",
            "email": "kixelated@gmail.com"
          },
          "committer": {
            "name": "GitHub",
            "username": "web-flow",
            "email": "noreply@github.com"
          },
          "id": "997ae1196c9b92de5a4e7cebb49318064d4e6df8",
          "message": "perf(net): a JS fill probe costs what changed, not every unknown run (#8550)\n\nCo-authored-by: Claude Opus 5.5 <noreply@anthropic.com>",
          "timestamp": "2026-11-12T15:07:17Z",
          "url": "https://github.com/moq-dev/moq/commit/997ae1196c9b92de5a4e7cebb49318064d4e6df8"
        },
        "date": 1794506673448,
        "tool": "customBiggerIsBetter",
        "benches": [
          {
            "name": "bench-rust-test/rust-test/moq-tokio/receive_credit/benchmark_receive_windows/path=loopback/axis=default/streams=16/frame=262144/window_mib=4/stream_window=finite/mib_per_s",
            "value": 442,
            "unit": "MiB/s"
          },
          {
            "name": "bench-rust-test/rust-test/moq-tokio/receive_credit/benchmark_receive_windows/path=loopback/axis=default/streams=16/frame=262144/window_mib=4/stream_window=finite/delivered_frames",
            "value": 1024,
            "unit": "frames"
          },
          {
            "name": "bench-rust-test/rust-test/moq-tokio/receive_credit/benchmark_receive_windows/path=bufferbloat/axis=default/streams=16/frame=262144/window_mib=4/stream_window=finite/mib_per_s",
            "value": 20,
            "unit": "MiB/s"
          },
          {
            "name": "bench-rust-test/rust-test/moq-tokio/receive_credit/benchmark_receive_windows/path=bufferbloat/axis=default/streams=16/frame=262144/window_mib=4/stream_window=finite/delivered_frames",
            "value": 64,
            "unit": "frames"
          },
          {
            "name": "bench-rust-test/rust-test/moq-tokio/receive_credit/benchmark_receive_windows/path=loopback/axis=streams/streams=1/frame=262144/window_mib=4/stream_window=finite/mib_per_s",
            "value": 441,
            "unit": "MiB/s"
          },
          {
            "name": "bench-rust-test/rust-test/moq-tokio/receive_credit/benchmark_receive_windows/path=loopback/axis=streams/streams=1/frame=262144/window_mib=4/stream_window=finite/delivered_frames",
            "value": 1024,
            "unit": "frames"
          },
          {
            "name": "bench-rust-test/rust-test/moq-tokio/receive_credit/benchmark_receive_windows/path=bufferbloat/axis=streams/streams=1/frame=262144/window_mib=4/stream_window=finite/mib_per_s",
            "value": 19,
            "unit": "MiB/s"
          },
          {
            "name": "bench-rust-test/rust-test/moq-tokio/receive_credit/benchmark_receive_windows/path=bufferbloat/axis=streams/streams=1/frame=262144/window_mib=4/stream_window=finite/delivered_frames",
            "value": 64,
            "unit": "frames"
          },
          {
            "name": "bench-rust-test/rust-test/moq-tokio/receive_credit/benchmark_receive_windows/path=loopback/axis=frame/streams=16/frame=16384/window_mib=4/stream_window=finite/mib_per_s",
            "value": 217,
            "unit": "MiB/s"
          },
          {
            "name": "bench-rust-test/rust-test/moq-tokio/receive_credit/benchmark_receive_windows/path=loopback/axis=frame/streams=16/frame=16384/window_mib=4/stream_window=finite/delivered_frames",
            "value": 16384,
            "unit": "frames"
          },
          {
            "name": "bench-rust-test/rust-test/moq-tokio/receive_credit/benchmark_receive_windows/path=bufferbloat/axis=frame/streams=16/frame=16384/window_mib=4/stream_window=finite/mib_per_s",
            "value": 20,
            "unit": "MiB/s"
          },
          {
            "name": "bench-rust-test/rust-test/moq-tokio/receive_credit/benchmark_receive_windows/path=bufferbloat/axis=frame/streams=16/frame=16384/window_mib=4/stream_window=finite/delivered_frames",
            "value": 1024,
            "unit": "frames"
          },
          {
            "name": "bench-rust-test/rust-test/moq-tokio/receive_credit/benchmark_receive_windows/path=loopback/axis=window/streams=16/frame=262144/window_mib=8/stream_window=finite/mib_per_s",
            "value": 448,
            "unit": "MiB/s"
          },
          {
            "name": "bench-rust-test/rust-test/moq-tokio/receive_credit/benchmark_receive_windows/path=loopback/axis=window/streams=16/frame=262144/window_mib=8/stream_window=finite/delivered_frames",
            "value": 1024,
            "unit": "frames"
          },
          {
            "name": "bench-rust-test/rust-test/moq-tokio/receive_credit/benchmark_receive_windows/path=bufferbloat/axis=window/streams=16/frame=262144/window_mib=8/stream_window=finite/mib_per_s",
            "value": 20,
            "unit": "MiB/s"
          },
          {
            "name": "bench-rust-test/rust-test/moq-tokio/receive_credit/benchmark_receive_windows/path=bufferbloat/axis=window/streams=16/frame=262144/window_mib=8/stream_window=finite/delivered_frames",
            "value": 64,
            "unit": "frames"
          },
          {
            "name": "bench-rust-test/rust-test/moq-tokio/receive_credit/benchmark_receive_windows/path=loopback/axis=window/streams=16/frame=262144/window_mib=32/stream_window=finite/mib_per_s",
            "value": 454,
            "unit": "MiB/s"
          },
          {
            "name": "bench-rust-test/rust-test/moq-tokio/receive_credit/benchmark_receive_windows/path=loopback/axis=window/streams=16/frame=262144/window_mib=32/stream_window=finite/delivered_frames",
            "value": 1024,
            "unit": "frames"
          },
          {
            "name": "bench-rust-test/rust-test/moq-tokio/receive_credit/benchmark_receive_windows/path=bufferbloat/axis=window/streams=16/frame=262144/window_mib=32/stream_window=finite/mib_per_s",
            "value": 20,
            "unit": "MiB/s"
          },
          {
            "name": "bench-rust-test/rust-test/moq-tokio/receive_credit/benchmark_receive_windows/path=bufferbloat/axis=window/streams=16/frame=262144/window_mib=32/stream_window=finite/delivered_frames",
            "value": 64,
            "unit": "frames"
          }
        ]
      },
      {
        "commit": {
          "author": {
            "name": "Luke Curley",
            "username": "kixelated",
            "email": "kixelated@gmail.com"
          },
          "committer": {
            "name": "GitHub",
            "username": "web-flow",
            "email": "noreply@github.com"
          },
          "id": "7df2ebf21cb053871fb5a61571789697879c002f",
          "message": "feat(net): a reader on a failed copy continues on the held re-offer (#8546)\n\nCo-authored-by: Claude Opus 5.5 <noreply@anthropic.com>",
          "timestamp": "2026-11-13T13:47:35Z",
          "url": "https://github.com/moq-dev/moq/commit/7df2ebf21cb053871fb5a61571789697879c002f"
        },
        "date": 1794588563878,
        "tool": "customBiggerIsBetter",
        "benches": [
          {
            "name": "bench-rust-test/rust-test/moq-tokio/receive_credit/benchmark_receive_windows/path=loopback/axis=default/streams=16/frame=262144/window_mib=4/stream_window=finite/mib_per_s",
            "value": 463,
            "unit": "MiB/s"
          },
          {
            "name": "bench-rust-test/rust-test/moq-tokio/receive_credit/benchmark_receive_windows/path=loopback/axis=default/streams=16/frame=262144/window_mib=4/stream_window=finite/delivered_frames",
            "value": 1024,
            "unit": "frames"
          },
          {
            "name": "bench-rust-test/rust-test/moq-tokio/receive_credit/benchmark_receive_windows/path=bufferbloat/axis=default/streams=16/frame=262144/window_mib=4/stream_window=finite/mib_per_s",
            "value": 20,
            "unit": "MiB/s"
          },
          {
            "name": "bench-rust-test/rust-test/moq-tokio/receive_credit/benchmark_receive_windows/path=bufferbloat/axis=default/streams=16/frame=262144/window_mib=4/stream_window=finite/delivered_frames",
            "value": 64,
            "unit": "frames"
          },
          {
            "name": "bench-rust-test/rust-test/moq-tokio/receive_credit/benchmark_receive_windows/path=loopback/axis=streams/streams=1/frame=262144/window_mib=4/stream_window=finite/mib_per_s",
            "value": 457,
            "unit": "MiB/s"
          },
          {
            "name": "bench-rust-test/rust-test/moq-tokio/receive_credit/benchmark_receive_windows/path=loopback/axis=streams/streams=1/frame=262144/window_mib=4/stream_window=finite/delivered_frames",
            "value": 1024,
            "unit": "frames"
          },
          {
            "name": "bench-rust-test/rust-test/moq-tokio/receive_credit/benchmark_receive_windows/path=bufferbloat/axis=streams/streams=1/frame=262144/window_mib=4/stream_window=finite/mib_per_s",
            "value": 20,
            "unit": "MiB/s"
          },
          {
            "name": "bench-rust-test/rust-test/moq-tokio/receive_credit/benchmark_receive_windows/path=bufferbloat/axis=streams/streams=1/frame=262144/window_mib=4/stream_window=finite/delivered_frames",
            "value": 64,
            "unit": "frames"
          },
          {
            "name": "bench-rust-test/rust-test/moq-tokio/receive_credit/benchmark_receive_windows/path=loopback/axis=frame/streams=16/frame=16384/window_mib=4/stream_window=finite/mib_per_s",
            "value": 224,
            "unit": "MiB/s"
          },
          {
            "name": "bench-rust-test/rust-test/moq-tokio/receive_credit/benchmark_receive_windows/path=loopback/axis=frame/streams=16/frame=16384/window_mib=4/stream_window=finite/delivered_frames",
            "value": 16384,
            "unit": "frames"
          },
          {
            "name": "bench-rust-test/rust-test/moq-tokio/receive_credit/benchmark_receive_windows/path=bufferbloat/axis=frame/streams=16/frame=16384/window_mib=4/stream_window=finite/mib_per_s",
            "value": 20,
            "unit": "MiB/s"
          },
          {
            "name": "bench-rust-test/rust-test/moq-tokio/receive_credit/benchmark_receive_windows/path=bufferbloat/axis=frame/streams=16/frame=16384/window_mib=4/stream_window=finite/delivered_frames",
            "value": 1024,
            "unit": "frames"
          },
          {
            "name": "bench-rust-test/rust-test/moq-tokio/receive_credit/benchmark_receive_windows/path=loopback/axis=window/streams=16/frame=262144/window_mib=8/stream_window=finite/mib_per_s",
            "value": 461,
            "unit": "MiB/s"
          },
          {
            "name": "bench-rust-test/rust-test/moq-tokio/receive_credit/benchmark_receive_windows/path=loopback/axis=window/streams=16/frame=262144/window_mib=8/stream_window=finite/delivered_frames",
            "value": 1024,
            "unit": "frames"
          },
          {
            "name": "bench-rust-test/rust-test/moq-tokio/receive_credit/benchmark_receive_windows/path=bufferbloat/axis=window/streams=16/frame=262144/window_mib=8/stream_window=finite/mib_per_s",
            "value": 20,
            "unit": "MiB/s"
          },
          {
            "name": "bench-rust-test/rust-test/moq-tokio/receive_credit/benchmark_receive_windows/path=bufferbloat/axis=window/streams=16/frame=262144/window_mib=8/stream_window=finite/delivered_frames",
            "value": 64,
            "unit": "frames"
          },
          {
            "name": "bench-rust-test/rust-test/moq-tokio/receive_credit/benchmark_receive_windows/path=loopback/axis=window/streams=16/frame=262144/window_mib=32/stream_window=finite/mib_per_s",
            "value": 456,
            "unit": "MiB/s"
          },
          {
            "name": "bench-rust-test/rust-test/moq-tokio/receive_credit/benchmark_receive_windows/path=loopback/axis=window/streams=16/frame=262144/window_mib=32/stream_window=finite/delivered_frames",
            "value": 1024,
            "unit": "frames"
          },
          {
            "name": "bench-rust-test/rust-test/moq-tokio/receive_credit/benchmark_receive_windows/path=bufferbloat/axis=window/streams=16/frame=262144/window_mib=32/stream_window=finite/mib_per_s",
            "value": 20,
            "unit": "MiB/s"
          },
          {
            "name": "bench-rust-test/rust-test/moq-tokio/receive_credit/benchmark_receive_windows/path=bufferbloat/axis=window/streams=16/frame=262144/window_mib=32/stream_window=finite/delivered_frames",
            "value": 64,
            "unit": "frames"
          }
        ]
      }
    ]
  }
}