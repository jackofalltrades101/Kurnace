using Godot;

// C# Godot 4.7 shot. The render module drives frames with `--fixed-fps` and
// `--write-movie`; rotation is a pure function of the process frame.
public partial class Shot : Node3D
{
    [Export] public float Spin = 0.075f;

    public override void _Process(double delta)
    {
        var box = GetNode<Node3D>("Box");
        var f = (float)Engine.GetProcessFrames();
        box.Rotation = new Vector3(0.22f, f * Spin, 0.10f);
    }
}
